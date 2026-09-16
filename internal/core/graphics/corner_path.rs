// Copyright © Klarälvdalens Datakonsult AB, a KDAB Group company , info@kdab.com, author Robin Cramer <robin.cramer@kdab.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore scanlines underflows

/*!
Builds an exact path for a rectangle with a `CornerShape` per corner, for renderers
(Skia, FemtoVG) that draw paths rather than rasterize scanlines. Callers with only
circular corners should keep using their native rounded-rect primitive; this is for
shapes that primitive can't express.
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
/// A high `|k|` needs depth, not breadth: its fillet is only a thousandth of `s` wide,
/// and bisection must reach that before the spans either side are accepted.
const MAX_SUPERELLIPSE_DEPTH: u32 = 12;

/// Cosine of the most a single cubic may turn through, here 30 degrees.
/// Sampling a span only bounds the error at the sampled points, so a sharply bending
/// span is split on turn alone — also what drives bisection down to a high `|k|`'s fillet.
const SUPERELLIPSE_TURN_COS: f32 = 0.866;

/// Places `(p, q)` in the corner's frame: offsets from the tip along `e_in` and `e_out`, in
/// units of `r`. A negative `k` is the convex curve mirrored about the bevel chord.
fn superellipse_corner(k: f32, p: f32, q: f32) -> Vector {
    if k < 0.0 { Vector::new(q, p) } else { Vector::new(1.0 - p, 1.0 - q) }
}

/// A point on the superellipse and the unit tangent there, in the corner's frame.
///
/// `s` runs from 0 at the incoming edge to 1 at the outgoing, placing the point where the
/// ray of slope `(1 - s) / s` crosses the curve. The CSS spec instead uses
/// `t` → `(1 - t^(1/n), 1 - (1-t)^(1/n))`, which underflows to 0 across most of the curve
/// for large `n`; `s` stays well conditioned at every `n`.
fn superellipse_at(k: f32, n: f32, s: f32) -> (Vector, Vector) {
    let (p, q) = superellipse_pq(n, s);
    // Perpendicular to the curve's implicit gradient, which stays finite where the slope
    // of `x` against `y` doesn't.
    let (gp, gq) = (p.powf(n - 1.0), q.powf(n - 1.0));
    let tangent = if k < 0.0 { Vector::new(-gp, gq) } else { Vector::new(-gq, gp) };
    (superellipse_corner(k, p, q), tangent.normalize())
}

/// How far `v` lies off the curve, in units of `r`.
///
/// Measured to the point the curve reaches along the same ray, so it never reports less
/// than the true distance — never accepted on its own to fit a span.
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
        CornerShape::Squircle => emit_superellipse(b, tip, e_in, e_out, r, SQUIRCLE_K),
        CornerShape::Superellipse(k) => emit_superellipse(b, tip, e_in, e_out, r, k),
    }
}

/// The curve's true perpendicular offset, `dn` outward (in units of `r`, negative moves
/// inward), at ray parameter `s`. See [`SpreadCorner`].
fn superellipse_offset_point(k: f32, n: f32, dn: f32, s: f32) -> Vector {
    let (pos, tangent) = superellipse_at(k, n, s);
    // Outward normal: tangent rotated 90 degrees, away from the material.
    pos + Vector::new(-tangent.y, tangent.x) * dn
}

/// Like [`emit_superellipse_span`], but fits cubics to the curve's own offset. There's no
/// closed-form distance to the offset curve, so this measures against the offset point at
/// the same ray parameter rather than the true nearest point — close enough at the
/// tolerances this runs at.
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
/// point a shrinking `spread` has outrun the curve's own curvature, so the curve doubles
/// back over its mirror image and both halves meet here instead.
///
/// Returns `Some(fold)` up to `0.5` while the curve doesn't fold at all, `None` once the
/// fold swallows the whole corner.
///
/// Bisects `x - y`, which starts at `1 - |spread| / r` and is `0` at `s == 0.5` by
/// symmetry.
fn fold_trim(k: f32, n: f32, dn: f32) -> Option<f32> {
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

/// Bisects `covered` — true up to one change-over, false past it — for that change-over
/// in `[0, limit]`. Returns `0` if never covered, `limit` if always covered.
///
/// Never asks `covered` about `s == 0`: there the offset point sits exactly on the edge
/// it's measured against, and rounding decides which side it lands on.
fn trim_at(limit: f32, covered: impl Fn(f32) -> bool) -> f32 {
    let (mut lo, mut hi) = (0.0f32, limit);
    for _ in 0..30 {
        let mid = (lo + hi) * 0.5;
        if covered(mid) { lo = mid } else { hi = mid }
    }
    hi
}

/// Where the offset curve clears the incoming edge and stays `|dn|` away — the corner
/// starts here rather than at `s == 0`, since the straight run already covers the rest.
///
/// The edge is a ray, so a point past its end measures to the corner's own vertex instead
/// — which is exactly what a folded offset does, and what this catches: erosion sweeps a
/// concave corner's curve past the edge, and growth folds an `n < 2` corner over itself.
fn edge_trim(k: f32, n: f32, dn: f32, fold: f32) -> f32 {
    trim_at(fold, |s| {
        let v = superellipse_offset_point(k, n, dn, s);
        // The edge runs from `(1, 0)`; a `v` short of that end measures to it.
        let d = if v.x >= 1.0 { v.y.abs() } else { (v - Vector::new(1.0, 0.0)).length() };
        d < dn.abs()
    })
}

/// The `superellipse(k)` parameter for a `Scoop`, `Squircle`, or `Superellipse` corner.
fn superellipse_k(shape: CornerShape) -> f32 {
    match shape {
        CornerShape::Scoop => -1.0,
        CornerShape::Squircle => SQUIRCLE_K,
        CornerShape::Superellipse(k) => k,
        _ => unreachable!(),
    }
}

/// One corner of a shape spread outward by `spread`, resolved once so
/// [`spread_rounded_rect_path`] can ask it for its [`start`](Self::start), its geometry,
/// and its [`end`](Self::end) before drawing the straight run into it.
#[derive(Clone, Copy)]
struct SpreadCorner {
    /// The *un*grown rectangle's own corner, anchoring every [`SpreadCornerKind`] below.
    tip: Point,
    /// Unit vectors along the incoming and outgoing straight edges. See [`emit_corner`].
    e_in: Vector,
    e_out: Vector,
    spread: f32,
    kind: SpreadCornerKind,
}

#[derive(Clone, Copy)]
enum SpreadCornerKind {
    /// A corner of the same shape, at the radius [`CornerShape::spread_scale`] gives —
    /// every shape except `Scoop`, `Squircle`, and `Superellipse`, whose true offset isn't
    /// itself a superellipse.
    Scaled { shape: CornerShape, r: f32 },
    /// A `superellipse(k)` corner's true offset curve, over `[start, fold]` and its mirror
    /// `[1 - fold, 1 - start]`, joined to each edge by a [miter](SpreadCorner::miter).
    Offset { k: f32, n: f32, r: f32, start: f32, fold: f32 },
    /// Nothing of the corner survives the spread, so the two straight runs meet in a point.
    Gone,
}

impl SpreadCorner {
    /// The grown rectangle's own corner — where a [`Scaled`](SpreadCornerKind::Scaled)
    /// corner sits, or where the two straight runs meet if nothing survives.
    fn grown_tip(&self) -> Point {
        self.tip - (self.e_in + self.e_out) * self.spread
    }

    /// `v`, in units of `r` along the corner's own two edges, as a point.
    fn to_point(&self, r: f32, v: Vector) -> Point {
        self.tip + self.e_in * (v.x * r) + self.e_out * (v.y * r)
    }

    /// The same corner with its edges swapped — a reflection about its own diagonal, which
    /// symmetry makes [`Self::end`] the mirror of [`Self::start`].
    fn mirrored(&self) -> Self {
        Self { e_in: self.e_out, e_out: self.e_in, ..*self }
    }

    /// Where the offset curve picks up, at `start`.
    fn on_curve(&self, k: f32, n: f32, r: f32, start: f32) -> Point {
        self.to_point(r, superellipse_offset_point(k, n, self.spread / r, start))
    }

    /// Where the incoming edge's offset line (`spread` out) meets the offset curve's
    /// tangent at `start`.
    ///
    /// Only a grown concave corner needs this: its curve meets the edge at a right angle no
    /// offset reaches, so a miter keeps it sharp rather than filleting it, matching both
    /// CSS's `box-shadow` spread and `Notch`'s own right angle. Every other corner's
    /// `start` is already on the line and comes back unchanged.
    ///
    /// Uses the curve's tangent, not the offset's: the two are parallel until the offset
    /// folds, and [`edge_trim`] already keeps `start` clear of that.
    fn miter(&self, k: f32, n: f32, r: f32, start: f32) -> Point {
        let from = self.on_curve(k, n, r, start);
        let tangent = superellipse_at(k, n, start).1;
        let direction = self.e_in * tangent.x + self.e_out * tangent.y;
        // How far back along the tangent the edge's offset line lies. A tangent along it
        // never meets it.
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

    /// Draws the corner, from [`Self::start`] — which the builder must already be at — to
    /// [`Self::end`].
    fn emit(&self, b: &mut impl SvgPathBuilder) {
        match self.kind {
            SpreadCornerKind::Gone => {}
            SpreadCornerKind::Scaled { shape, r } => {
                emit_corner(b, self.grown_tip(), self.e_in, self.e_out, r, shape)
            }
            SpreadCornerKind::Offset { k, n, r, start, fold } => {
                let dn = self.spread / r;
                let to_point = |v: Vector| self.to_point(r, v);
                // Degenerate unless the corner is a grown concave one. See [`Self::miter`].
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

/// Resolves the corner at `tip`, the *grown* rectangle's corner, of a shape spread
/// outward by `spread`. `r`, `shape`, and `max_r` describe the ungrown rectangle.
///
/// A `superellipse(k)` corner draws its own offset curve, trimmed by [`fold_trim`] and
/// [`edge_trim`]; every other shape reuses itself at the radius
/// [`CornerShape::spread_scale`] gives.
fn spread_corner(
    tip: Point,
    e_in: Vector,
    e_out: Vector,
    r: f32,
    shape: CornerShape,
    spread: f32,
    max_r: f32,
) -> SpreadCorner {
    let corner =
        |kind| SpreadCorner { tip: tip + (e_in + e_out) * spread, e_in, e_out, spread, kind };
    let r = if shape == CornerShape::Square { 0.0 } else { r.clamp(0.0, max_r) };
    match shape {
        CornerShape::Scoop | CornerShape::Squircle | CornerShape::Superellipse(_) if r > 0.0 => {
            let k = superellipse_k(shape);
            let n = superellipse_n_from_k(k);
            let dn = spread / r;
            match fold_trim(k, n, dn).map(|fold| (edge_trim(k, n, dn, fold), fold)) {
                Some((start, fold)) if start < fold => {
                    corner(SpreadCornerKind::Offset { k, n, r, start, fold })
                }
                // Growing a concave corner until it closes over leaves its two walls,
                // still `r` deep — a `Notch`, the limit this shape approaches.
                _ if k < 0.0 && spread > 0.0 => {
                    corner(SpreadCornerKind::Scaled { shape: CornerShape::Notch, r })
                }
                _ => corner(SpreadCornerKind::Gone),
            }
        }
        // A corner with no radius stays sharp, per CSS's `box-shadow` spread.
        _ if r <= 0.0 => corner(SpreadCornerKind::Gone),
        _ => corner(SpreadCornerKind::Scaled {
            shape,
            r: (r + spread * shape.spread_scale()).clamp(0.0, (max_r + spread).max(0.0)),
        }),
    }
}

/// Builds the path for a rectangle grown outward by `spread` from one at `(x, y)` sized
/// `width` x `height`, with per-corner `radius` and `corner_shape` on the *un*grown
/// rectangle. `(x, y, width, height)` are the grown geometry (e.g. `width + 2 * spread`).
///
/// A negative `spread` shrinks the shape instead, for an inner shadow's hole. See
/// [`spread_corner`] for how each corner grows; a plain [`rounded_rect_path`] at this same
/// geometry with `radius` scaled up is only exact for `Round`.
pub fn spread_rounded_rect_path<U>(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: BorderRadius<f32, U>,
    corner_shape: CornerShapes,
    spread: f32,
) -> lyon_path::Path {
    // `radius` describes the ungrown rectangle, so it's capped against that one's size.
    let max_r = (width.min(height) / 2.0 - spread).max(0.0);
    let corner =
        |tip, e_in, e_out, r, shape| spread_corner(tip, e_in, e_out, r, shape, spread, max_r);
    let tl = corner(
        Point::new(x, y),
        Vector::new(0.0, 1.0),
        Vector::new(1.0, 0.0),
        radius.top_left,
        corner_shape.top_left,
    );
    let tr = corner(
        Point::new(x + width, y),
        Vector::new(-1.0, 0.0),
        Vector::new(0.0, 1.0),
        radius.top_right,
        corner_shape.top_right,
    );
    let br = corner(
        Point::new(x + width, y + height),
        Vector::new(0.0, -1.0),
        Vector::new(-1.0, 0.0),
        radius.bottom_right,
        corner_shape.bottom_right,
    );
    let bl = corner(
        Point::new(x, y + height),
        Vector::new(1.0, 0.0),
        Vector::new(0.0, -1.0),
        radius.bottom_left,
        corner_shape.bottom_left,
    );

    let mut b = lyon_path::Path::builder().with_svg();
    // `top_left` is emitted last, so the path starts where its own corner ends, matching
    // `rounded_rect_path`'s `move_to(x + r_tl, y)`.
    b.move_to(tl.end());
    for corner in [tr, br, bl, tl] {
        b.line_to(corner.start());
        corner.emit(&mut b);
    }
    b.close();
    b.build()
}

/// Builds the path for a rectangle at `(x, y)` of size `width` x `height`, with per-corner
/// `radius` and `corner_shape`. [`CornerShape::Square`] renders sharp regardless of its
/// configured radius.
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

    /// Samples `path` into a dense polyline.
    fn sample_path(path: &lyon_path::Path) -> Vec<Point> {
        const SAMPLES: u32 = 128;
        let mut out = Vec::new();
        for event in path.iter() {
            match event {
                lyon_path::Event::Begin { at } => out.push(at),
                lyon_path::Event::Line { from, to } => {
                    out.extend((1..=SAMPLES).map(|i| from.lerp(to, i as f32 / SAMPLES as f32)));
                }
                lyon_path::Event::Quadratic { to, .. } => out.push(to),
                lyon_path::Event::Cubic { from, ctrl1, ctrl2, to } => {
                    out.extend((1..=SAMPLES).map(|i| {
                        let t = i as f32 / SAMPLES as f32;
                        let u = 1.0 - t;
                        (from.to_vector() * (u * u * u)
                            + ctrl1.to_vector() * (3.0 * u * u * t)
                            + ctrl2.to_vector() * (3.0 * u * t * t)
                            + to.to_vector() * (t * t * t))
                            .to_point()
                    }));
                }
                lyon_path::Event::End { .. } => {}
            }
        }
        out
    }

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
        sample_path(&path)
            .into_iter()
            .map(|p| Vector::new(2.0 * r - p.x, p.y))
            .filter(|local| local.x >= -0.001 && local.x <= r + 0.001 && local.y <= r + 0.001)
            .collect()
    }

    /// Samples `at` over `[0, r]`, bisecting wherever the chord between two samples strays
    /// from the true halfway point — so the polyline tracks the curve however sharply it
    /// turns or unevenly `d` runs along it.
    fn adaptive(r: f32, at: impl Fn(f32) -> Vector) -> Vec<Vector> {
        // A twelfth of the asserted deviation, so the reference's own error can't be what
        // a failure reports.
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
    /// Sampled along both axes and merged, since `corner_boundary` is its own inverse for
    /// every shape here: a `y`-only grid would span the near-vertical stretch — most of a
    /// high-`|k|` corner, and both walls of a `Notch` — with a single chord.
    fn reference_corner(shape: CornerShape, r: f32) -> Vec<Vector> {
        // `Square` draws with no radius regardless of its configured one, so its window
        // is bounded by the two edges meeting at the tip.
        let effective = if shape == CornerShape::Square { 0.0 } else { r };
        // `Notch` alone has a discontinuous boundary, stepping from `r` to 0 at `y == r`,
        // so no finite sampling lands on the right angle its two walls make.
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

    const SHAPES: [CornerShape; 15] = [
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

    #[test]
    fn corner_path_matches_corner_boundary() {
        for shape in SHAPES {
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

    /// The whole ungrown rectangle's boundary as a dense polyline, from the same
    /// [`reference_corner`] the corner test measures against.
    fn reference_boundary(shape: CornerShape, r: f32, w: f32, h: f32) -> Vec<Vector> {
        let corner = reference_corner(shape, r);
        // The same four corners, in the same order, that `spread_rounded_rect_path` walks.
        let corners = [
            (Vector::new(w, 0.0), Vector::new(-1.0, 0.0), Vector::new(0.0, 1.0)),
            (Vector::new(w, h), Vector::new(0.0, -1.0), Vector::new(-1.0, 0.0)),
            (Vector::new(0.0, h), Vector::new(1.0, 0.0), Vector::new(0.0, -1.0)),
            (Vector::new(0.0, 0.0), Vector::new(0.0, 1.0), Vector::new(1.0, 0.0)),
        ];
        let mut out: Vec<Vector> = corners
            .iter()
            .flat_map(|&(tip, e_in, e_out)| {
                corner.iter().map(move |v| tip + e_in * v.x + e_out * v.y)
            })
            .collect();
        out.push(out[0]);
        out
    }

    /// A spread's whole point is an even-width band, so every point of the path must stay
    /// `spread` away from the shape it spreads from — except at a miter, where two offset
    /// runs meet at a right angle, the sharpest this module draws, reaching
    /// `spread * sqrt(2)`.
    #[test]
    fn spread_path_keeps_its_distance() {
        const TOLERANCE: f32 = 0.1;
        let (w, h) = (200.0f32, 160.0f32);
        for shape in SHAPES {
            for r in [8.0f32, 40.0] {
                for spread in [-16.0f32, -4.0, 4.0, 16.0] {
                    let path = spread_rounded_rect_path(
                        -spread,
                        -spread,
                        w + 2.0 * spread,
                        h + 2.0 * spread,
                        BorderRadius::<f32, ()>::new_uniform(r),
                        CornerShapes::new_uniform(shape),
                        spread,
                    );
                    let reference = reference_boundary(shape, r, w, h);
                    for p in sample_path(&path) {
                        let d = distance_to_polyline(p.to_vector(), &reference);
                        assert!(
                            d >= spread.abs() - TOLERANCE,
                            "{shape:?} r={r} spread={spread}: {p:?} is only {d} from the shape"
                        );
                        assert!(
                            d <= spread.abs() * core::f32::consts::SQRT_2 + TOLERANCE,
                            "{shape:?} r={r} spread={spread}: {p:?} is {d} from the shape"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn superellipse_k_matches_its_named_shape() {
        // Every named shape is one CSS superellipse parameter, so the two spellings must
        // agree. `Scoop` in particular is `superellipse(-1)`, not the `n < 1` curve
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
