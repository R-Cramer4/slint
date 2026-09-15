// Copyright © Klarälvdalens Datakonsult AB, a KDAB Group company , info@kdab.com, author Robin Cramer <robin.cramer@kdab.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore scanlines underflows

/*!
Builds an exact path for a rectangle whose corners are cut according to a
`CornerShape` per corner, for renderers (Skia, FemtoVG) that draw paths rather than
rasterize scanlines directly. Callers with only circular corners should keep using
their native rounded-rect primitive instead; this is for the shapes that primitive
can't express.
*/

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use num_traits::Float;

use super::BorderRadius;
use super::border_radius::CornerShape;
use super::border_radius::{SQUIRCLE_K, superellipse_n_from_k, superellipse_pq};
use crate::lengths::CornerShapes;
use lyon_path::math::{Point, Vector};
use lyon_path::traits::SvgPathBuilder;

/// The standard cubic-bezier approximation factor for a quarter circle.
const KAPPA: f32 = 0.552_284_7;

/// Largest deviation, in physical pixels, allowed between an emitted superellipse corner
/// and the true curve.
const SUPERELLIPSE_TOLERANCE: f32 = 0.05;

/// Deepest a span is halved by [`emit_superellipse`].
/// A high `|k|` needs the depth rather than the breadth: its fillet is only a thousandth
/// of `s` wide. Bisection has to reach that before the spans either side of it are
/// accepted.
const MAX_SUPERELLIPSE_DEPTH: u32 = 12;

/// Cosine of the most a single cubic may turn through, here 30 degrees.
/// Sampling a span only bounds the error at the points sampled, so a span that bends
/// sharply is split on that alone. That's also what drives the bisection down to a high
/// `|k|`'s fillet.
const SUPERELLIPSE_TURN_COS: f32 = 0.866;

/// Places `(p, q)` in the corner's frame: offsets from the tip along `e_in` and `e_out`, in
/// units of `r`. A negative `k` is the convex curve mirrored about the bevel chord.
fn superellipse_corner(k: f32, p: f32, q: f32) -> Vector {
    if k < 0.0 { Vector::new(q, p) } else { Vector::new(1.0 - p, 1.0 - q) }
}

/// A point on the superellipse and the unit tangent there, in the corner's frame.
///
/// `s` runs from 0 at the incoming edge to 1 at the outgoing one, placing the point where
/// the ray of slope `(1 - s) / s` crosses the curve.
/// The CSS spec instead parametrizes it as `(1 - t^(1/n), 1 - (1-t)^(1/n))`. That `t`
/// underflows to 0 across most of the curve once `n` is large, whereas `s` stays well
/// conditioned at every `n`.
fn superellipse_at(k: f32, n: f32, s: f32) -> (Vector, Vector) {
    let (p, q) = superellipse_pq(n, s);
    // Perpendicular to the gradient of the curve's implicit form, which stays finite at
    // both ends where the slope of `x` against `y` doesn't.
    let (gp, gq) = (p.powf(n - 1.0), q.powf(n - 1.0));
    let tangent = if k < 0.0 { Vector::new(-gp, gq) } else { Vector::new(-gq, gp) };
    (superellipse_corner(k, p, q), tangent.normalize())
}

/// How far `v` lies off the curve, in units of `r`.
///
/// Measured to the point the curve reaches along the same ray, so this never reports less
/// than the true distance.
/// A span is never accepted on the strength of it alone.
fn superellipse_distance(k: f32, n: f32, v: Vector) -> f32 {
    let (p, q) = if k < 0.0 { (v.y, v.x) } else { (1.0 - v.x, 1.0 - v.y) };
    let total = p + q;
    let s = if total > 0.0 { (p / total).clamp(0.0, 1.0) } else { 0.5 };
    let (p, q) = superellipse_pq(n, s);
    (v - superellipse_corner(k, p, q)).length()
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
        // The tangents are parallel, so the span is straight and the system is singular.
        // Evenly spaced control points reproduce the line.
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
/// A superellipse has no closed-form Bezier, and no fixed schedule of segments suits the
/// whole `k` range.
/// Past about `|k| = 2` the corner is two nearly straight runs either side of a short
/// fillet. Spacing the segments evenly, by either length or turn, starves one of the two.
/// So the span is halved until each cubic is within tolerance of the curve.
fn emit_superellipse(
    b: &mut impl SvgPathBuilder,
    tip: Point,
    e_in: Vector,
    e_out: Vector,
    r: f32,
    k: f32,
) {
    let n = superellipse_n_from_k(k);
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
/// `tip + r * e_out`.
/// `e_in`/`e_out` are unit vectors from the tip along the incoming/outgoing straight
/// edges. The caller has already moved to `tip + r * e_in`.
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
        CornerShape::Squircle => emit_superellipse(b, tip, e_in, e_out, r, SQUIRCLE_K),
        CornerShape::Superellipse(k) => emit_superellipse(b, tip, e_in, e_out, r, k),
    }
}

/// Builds the path for a rectangle at `(x, y)` of size `width` x `height`, with per-corner
/// `radius` and `corner_shape`. A corner shaped [`CornerShape::Square`] renders with a sharp
/// angle regardless of its configured radius.
pub fn rounded_rect_path<U>(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: BorderRadius<f32, U>,
    corner_shape: CornerShapes,
) -> lyon_path::Path {
    let max_r = (width.min(height) / 2.0).max(0.0);
    let effective_radius = |r: f32, shape: CornerShape| {
        if shape == CornerShape::Square { 0.0 } else { r }.clamp(0.0, max_r)
    };
    let r_tl = effective_radius(radius.top_left, corner_shape.top_left);
    let r_tr = effective_radius(radius.top_right, corner_shape.top_right);
    let r_br = effective_radius(radius.bottom_right, corner_shape.bottom_right);
    let r_bl = effective_radius(radius.bottom_left, corner_shape.bottom_left);

    let mut b = lyon_path::Path::builder().with_svg();
    b.move_to(Point::new(x + r_tl, y));
    b.line_to(Point::new(x + width - r_tr, y));
    emit_corner(
        &mut b,
        Point::new(x + width, y),
        Vector::new(-1.0, 0.0),
        Vector::new(0.0, 1.0),
        r_tr,
        corner_shape.top_right,
    );
    b.line_to(Point::new(x + width, y + height - r_br));
    emit_corner(
        &mut b,
        Point::new(x + width, y + height),
        Vector::new(0.0, -1.0),
        Vector::new(-1.0, 0.0),
        r_br,
        corner_shape.bottom_right,
    );
    b.line_to(Point::new(x + r_bl, y + height));
    emit_corner(
        &mut b,
        Point::new(x, y + height),
        Vector::new(1.0, 0.0),
        Vector::new(0.0, -1.0),
        r_bl,
        corner_shape.bottom_left,
    );
    b.line_to(Point::new(x, y + r_tl));
    emit_corner(
        &mut b,
        Point::new(x, y),
        Vector::new(0.0, 1.0),
        Vector::new(1.0, 0.0),
        r_tl,
        corner_shape.top_left,
    );
    b.close();
    b.build()
}

#[cfg(all(test, feature = "path"))]
mod tests {
    use super::*;
    use crate::graphics::border_radius::corner_boundary;
    use alloc::vec::Vec;

    /// Samples the emitted path, in the corner-local frame of the top-right corner of a
    /// `2r` square: `.x` is the offset from the tip along the incoming edge, `.y` along the
    /// outgoing one, both in `0..=r`.
    fn emitted_corner(shape: CornerShape, r: f32) -> Vec<Vector> {
        let path = rounded_rect_path(
            0.0,
            0.0,
            2.0 * r,
            2.0 * r,
            BorderRadius::<f32, ()>::new_uniform(r),
            CornerShapes::new_uniform(shape),
        );
        let mut out = Vec::new();
        let mut push = |p: Point| {
            let local = Vector::new(2.0 * r - p.x, p.y);
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
                    for i in 1..=SAMPLES {
                        let t = i as f32 / SAMPLES as f32;
                        let u = 1.0 - t;
                        push(
                            (from.to_vector() * (u * u * u)
                                + ctrl1.to_vector() * (3.0 * u * u * t)
                                + ctrl2.to_vector() * (3.0 * u * t * t)
                                + to.to_vector() * (t * t * t))
                                .to_point(),
                        );
                    }
                }
                lyon_path::Event::End { .. } => {}
            }
        }
        out
    }

    /// Samples `at` over `[0, r]`, bisecting wherever the chord between two samples strays
    /// from the point halfway between them.
    /// So the polyline tracks the curve however sharply it turns and however unevenly `d`
    /// runs along it.
    fn adaptive(r: f32, at: impl Fn(f32) -> Vector) -> Vec<Vector> {
        // A twelfth of the deviation the test asserts, so the reference's own error can't
        // be what a failure is reporting.
        const MAX_SAGITTA: f32 = 0.004;
        let mut ds: Vec<f32> = (0..=32).map(|i| r * i as f32 / 32.0).collect();
        for _ in 0..12 {
            let mut split = false;
            let mut next = Vec::with_capacity(ds.len() * 2);
            for w in ds.windows(2) {
                next.push(w[0]);
                let mid = (w[0] + w[1]) * 0.5;
                if (at(mid) - (at(w[0]) + at(w[1])) * 0.5).length() > MAX_SAGITTA {
                    next.push(mid);
                    split = true;
                }
            }
            next.push(r);
            ds = next;
            if !split {
                break;
            }
        }
        ds.into_iter().map(at).collect()
    }

    /// The true corner, from [`corner_boundary`], in the same frame.
    ///
    /// `corner_boundary` is sampled along both axes and the two runs merged.
    /// It's its own inverse for every shape here, so a grid in `y` alone would span the
    /// near-vertical stretch with a single chord: most of a high-`|k|` corner, and both
    /// walls of a `Notch`.
    fn reference_corner(shape: CornerShape, r: f32) -> Vec<Vector> {
        // A `Square` corner is drawn with no radius whatever its configured one, so what
        // bounds its window is the two straight edges meeting at the tip.
        let effective = if shape == CornerShape::Square { 0.0 } else { r };
        // `Notch` alone has a discontinuous boundary: `corner_boundary` steps from `r` to
        // 0 at `y == r`. So no finite sampling of it lands on the right angle its two
        // walls make.
        let vertex = (shape == CornerShape::Notch).then_some(Vector::new(r, r));
        let f = |d: f32| corner_boundary(shape, effective, d);
        let mut out: Vec<Vector> = adaptive(r, |d| Vector::new(f(d), d))
            .into_iter()
            .chain(adaptive(r, |d| Vector::new(d, f(d))))
            .chain(vertex)
            .collect();
        // The corner is monotone, so ordering the merged samples puts them back in path
        // order.
        out.sort_by(|a, b| a.y.total_cmp(&b.y).then(b.x.total_cmp(&a.x)));
        out
    }

    fn distance_to_polyline(p: Vector, line: &[Vector]) -> f32 {
        line.windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                let t = if d.square_length() > 0.0 {
                    ((p - w[0]).dot(d) / d.square_length()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (p - (w[0] + d * t)).length()
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// The largest gap in either direction between the emitted path and the true corner.
    fn max_deviation(shape: CornerShape, r: f32) -> f32 {
        let emitted = emitted_corner(shape, r);
        let reference = reference_corner(shape, r);
        let forward = emitted.iter().map(|&p| distance_to_polyline(p, &reference));
        let backward = reference.iter().map(|&p| distance_to_polyline(p, &emitted));
        forward.chain(backward).fold(0.0, f32::max)
    }

    #[test]
    fn corner_path_matches_corner_boundary() {
        let shapes = [
            CornerShape::Square,
            CornerShape::Notch,
            CornerShape::Bevel,
            CornerShape::Round,
            CornerShape::Scoop,
            CornerShape::Squircle,
            CornerShape::Superellipse(-4.0),
            CornerShape::Superellipse(-2.0),
            CornerShape::Superellipse(-1.0),
            CornerShape::Superellipse(-0.5),
            CornerShape::Superellipse(0.0),
            CornerShape::Superellipse(0.5),
            CornerShape::Superellipse(1.0),
            CornerShape::Superellipse(3.0),
            CornerShape::Superellipse(6.0),
        ];
        for shape in shapes {
            for r in [4.0f32, 16.0, 64.0, 256.0] {
                // `Round` and `Scoop` are a single Bezier per corner, whose error against a
                // quarter circle grows with the radius rather than being budgeted.
                let tolerance = SUPERELLIPSE_TOLERANCE.max(3.0e-4 * r);
                let deviation = max_deviation(shape, r);
                assert!(
                    deviation <= tolerance,
                    "{shape:?} at r={r}: deviation {deviation} exceeds {tolerance}"
                );
            }
        }
    }

    #[test]
    fn superellipse_k_matches_its_named_shape() {
        // Per the CSS spec every named shape is one superellipse parameter, so the two
        // spellings have to agree.
        // `Scoop` in particular is `superellipse(-1)`, not the `n < 1` curve that
        // dropping the sign would give.
        for (named, k) in [
            (CornerShape::Bevel, 0.0),
            (CornerShape::Round, 1.0),
            (CornerShape::Squircle, 2.0),
            (CornerShape::Scoop, -1.0),
        ] {
            for i in 0..=64 {
                let y = 100.0 * i as f32 / 64.0;
                let named_x = corner_boundary(named, 100.0, y);
                let superellipse_x = corner_boundary(CornerShape::Superellipse(k), 100.0, y);
                assert!(
                    (named_x - superellipse_x).abs() < 0.02,
                    "{named:?} vs superellipse({k}) at y={y}: {named_x} vs {superellipse_x}"
                );
            }
        }
    }
}
