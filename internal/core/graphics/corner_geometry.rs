// Copyright © Klarälvdalens Datakonsult AB, a KDAB Group company , info@kdab.com, author Robin Cramer <robin.cramer@kdab.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
The outline of rectangle corners with a [`CornerShape`], shared by every renderer.

A border is drawn as a stroke of its centerline: the corner shape on the rectangle inset by
half the border width, see [`BorderRectLayout`](crate::item_rendering::BorderRectLayout).
Its two edges are that centerline offset by half the border width to either side, with miter
joins like a stroked path's.

Corners are computed in the *corner frame*: the corner's outer tip at the origin, `x` running
along the incoming edge and `y` along the outgoing one, so the rectangle lies in `x, y >= 0`.
Every corner shape is symmetric about the frame's diagonal.
*/

use super::border_radius::{BorderRadius, CornerShape, CornerShapes};
use alloc::vec::Vec;
use euclid::num::Zero;
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use num_traits::Float;

type Point = euclid::default::Point2D<f32>;
type Vector = euclid::default::Vector2D<f32>;

/// The CSS `superellipse(k)` parameter of [`CornerShape::Squircle`].
pub(crate) const SQUIRCLE_K: f32 = 2.0;

/// Larger `|k|` is clamped to this: `2^16` is indistinguishable from `Square` or `Notch`,
/// and an unclamped `n = 2^|k|` overflows.
const MAX_SUPERELLIPSE_K: f32 = 16.0;

/// Largest distance, in pixels, between a flattened curve and the true one.
const TOLERANCE: f32 = 0.02;

/// Most a flattened curve turns, in radians, between two segments.
const MAX_TURN: f32 = 0.3;

/// Deepest a superellipse span is halved while flattening.
const MAX_SUPERELLIPSE_DEPTH: u32 = 16;

/// The `superellipse(k)` parameter of `shape`, for the shapes without a closed form.
pub(crate) fn superellipse_k(shape: CornerShape) -> Option<f32> {
    match shape {
        CornerShape::Squircle => Some(SQUIRCLE_K),
        // A NaN `k` renders as `round`, the default shape.
        CornerShape::Superellipse(k) if k.is_nan() => Some(1.),
        CornerShape::Superellipse(k) => Some(k),
        _ => None,
    }
}

/// The superellipse exponent `n = 2^|k|`.
///
/// `k`'s sign selects the convex or the concave branch, not the exponent.
/// See [`superellipse_at`].
pub(crate) fn superellipse_n(k: f32) -> f32 {
    2f32.powf(k.abs().min(MAX_SUPERELLIPSE_K))
}

/// `(p, q)` on the ray of slope `(1 - s) / s`, scaled onto the curve so `p^n + q^n == 1`.
fn superellipse_pq(n: f32, s: f32) -> (f32, f32) {
    // Factoring out the larger keeps the power in `0..=1`, so a large `n` can't overflow it.
    let (larger, smaller) = (s.max(1. - s), s.min(1. - s));
    let scale = larger * (1. + (smaller / larger).powf(n)).powf(1. / n);
    (s / scale, (1. - s) / scale)
}

/// Places `(p, q)` in the corner frame, in units of the radius.
/// A negative `k` is the convex curve mirrored about the bevel chord.
fn superellipse_corner(k: f32, p: f32, q: f32) -> Vector {
    if k < 0. { Vector::new(q, p) } else { Vector::new(1. - p, 1. - q) }
}

/// A point on the `superellipse(k)` corner and the unit tangent there, in the corner frame
/// and in units of the radius.
///
/// Positive `k` gives the convex branch, `(1 - x)^n + (1 - y)^n = 1`, and negative `k` the
/// concave one, `x^n + y^n = 1`, so that `superellipse(-1)` is `Scoop`.
/// `s` runs from 0 at the incoming edge to 1 at the outgoing one, placing the point where the
/// ray of slope `(1 - s) / s` crosses the curve.
/// The CSS spec parametrizes by `t` → `(1 - t^(1/n), 1 - (1-t)^(1/n))` instead, which underflows
/// to 0 across most of the curve for a large `n`.
pub(crate) fn superellipse_at(k: f32, n: f32, s: f32) -> (Vector, Vector) {
    let (p, q) = superellipse_pq(n, s);
    // Perpendicular to the implicit gradient, which stays finite where the slope doesn't.
    let (gp, gq) = (p.powf(n - 1.), q.powf(n - 1.));
    let tangent = if k < 0. { Vector::new(-gp, gq) } else { Vector::new(-gq, gp) };
    (superellipse_corner(k, p, q), tangent.normalize())
}

/// Appends the corner of `shape` and radius `r` to `out`, as a polyline in the corner frame
/// from `(r, 0)` to `(0, r)`, turning at most `max_turn` between two segments of a curve.
fn flatten_corner(shape: CornerShape, r: f32, max_turn: f32, out: &mut Vec<Point>) {
    if r <= 0. || shape == CornerShape::Square {
        out.push(Point::zero());
        return;
    }
    let quarter = core::f32::consts::FRAC_PI_2;
    let arc_steps = || {
        let chord_turn = 2. * (1. - (TOLERANCE / r).min(1.)).acos();
        (quarter / max_turn.min(chord_turn)).ceil().max(1.) as u32
    };
    match shape {
        CornerShape::Notch => out.extend([Point::new(r, 0.), Point::new(r, r), Point::new(0., r)]),
        CornerShape::Bevel => out.extend([Point::new(r, 0.), Point::new(0., r)]),
        CornerShape::Round => {
            let steps = arc_steps();
            out.extend((0..=steps).map(|i| {
                let (sin, cos) = (quarter * i as f32 / steps as f32).sin_cos();
                Point::new(r - r * sin, r - r * cos)
            }));
        }
        CornerShape::Scoop => {
            let steps = arc_steps();
            out.extend((0..=steps).map(|i| {
                let (sin, cos) = (quarter * i as f32 / steps as f32).sin_cos();
                Point::new(r * cos, r * sin)
            }));
        }
        CornerShape::Square => unreachable!(),
        CornerShape::Squircle | CornerShape::Superellipse(_) => {
            let k = superellipse_k(shape).unwrap();
            let n = superellipse_n(k);
            let start = superellipse_at(k, n, 0.);
            out.push((start.0 * r).to_point());
            flatten_superellipse_span(
                out,
                (k, n, r, max_turn.cos()),
                (0., start),
                (1., superellipse_at(k, n, 1.)),
                MAX_SUPERELLIPSE_DEPTH,
            );
        }
    }
}

/// Appends the superellipse between `s0` and `s1`, excluding `s0` itself, halving the span
/// until it's within [`TOLERANCE`] and turns no more than `min_turn_cos`.
fn flatten_superellipse_span(
    out: &mut Vec<Point>,
    (k, n, r, min_turn_cos): (f32, f32, f32, f32),
    (s0, (p0, t0)): (f32, (Vector, Vector)),
    (s1, (p1, t1)): (f32, (Vector, Vector)),
    depth: u32,
) {
    let s = (s0 + s1) * 0.5;
    let middle = superellipse_at(k, n, s);
    let chord = (p1 - p0) * r;
    let offset = (middle.0 - p0) * r;
    let chord_error = if chord.square_length() > 0. {
        chord.cross(offset).abs() / chord.length()
    } else {
        offset.length()
    };
    if depth == 0 || (t0.dot(t1) >= min_turn_cos && chord_error <= TOLERANCE) {
        out.push((p1 * r).to_point());
    } else {
        let params = (k, n, r, min_turn_cos);
        flatten_superellipse_span(out, params, (s0, (p0, t0)), (s, middle), depth - 1);
        flatten_superellipse_span(out, params, (s, middle), (s1, (p1, t1)), depth - 1);
    }
}

/// The limit on a miter's length, relative to half the stroke width, past which it's
/// beveled, as in Skia's default stroke.
const MITER_LIMIT: f32 = 4.;

/// The joint where the offsets `a` (ending the segment along `da`) and `b` (starting the
/// segment along `db`) meet, on the outside of the turn at `vertex`, or `None` for a bevel.
fn miter(
    vertex: Point,
    a: Point,
    da: Vector,
    b: Point,
    db: Vector,
    distance: f32,
) -> Option<Point> {
    let cross = da.cross(db);
    if cross.abs() <= 1e-6 * da.length() * db.length() {
        return None;
    }
    let joint = a + da * ((b - a).cross(db) / cross);
    ((joint - vertex).length() <= MITER_LIMIT * distance).then_some(joint)
}

/// The area a stroke of half-width `distance` along `line` covers: a rectangle per segment,
/// and a miter (or bevel) on the outside of each turn.
struct Stroke<'a> {
    line: &'a [Point],
    distance: f32,
    /// The outline of each join, as a convex polygon.
    joins: Vec<[Point; 4]>,
}

impl<'a> Stroke<'a> {
    fn new(line: &'a [Point], distance: f32) -> Self {
        let joins = line
            .windows(3)
            .filter_map(|w| {
                let (before, vertex, after) = (w[0], w[1], w[2]);
                let (da, db) = ((vertex - before).normalize(), (after - vertex).normalize());
                let cross = da.cross(db);
                if cross.abs() < 1e-6 {
                    return None;
                }
                // Toward the outside of the turn.
                let side = if cross > 0. { -distance } else { distance };
                let a = vertex + Vector::new(-da.y, da.x) * side;
                let b = vertex + Vector::new(-db.y, db.x) * side;
                let joint = miter(vertex, a, da, b, db, distance).unwrap_or(b);
                Some([vertex, a, joint, b])
            })
            .collect();
        Self { line, distance, joins }
    }

    /// Whether `p` lies inside the stroke by more than `margin`.
    fn covers(&self, p: Point, margin: f32) -> bool {
        let reach = self.distance - margin;
        let in_segment = self.line.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let d = b - a;
            let length = d.length();
            if length <= 0. {
                return false;
            }
            let d = d / length;
            // Reaching slightly into the joins, so their seams count as covered.
            let along = (p - a).dot(d);
            along > -margin && along < length + margin && (p - a).cross(d).abs() < reach
        });
        in_segment
            || self.joins.iter().any(|join| {
                // Inside every edge of the convex join, whichever way it winds.
                let edges = [
                    (join[0], join[1]),
                    (join[1], join[2]),
                    (join[2], join[3]),
                    (join[3], join[0]),
                ];
                let sides = edges.map(|(u, v)| {
                    let e = v - u;
                    let len = e.length();
                    if len <= 0. { None } else { Some(e.cross(p - u) / len) }
                });
                let sides: Vec<f32> = sides.into_iter().flatten().collect();
                sides.iter().all(|s| *s > margin) || sides.iter().all(|s| *s < -margin)
            })
    }
}

/// Offsets `line` by `distance` to its left (outward for a corner traversed from its
/// incoming to its outgoing edge), or to its right for a negative `distance`.
///
/// On the outside of a turn, consecutive offset segments meet in a miter.
/// On the inside, and wherever the offset folds back over itself past a curve's radius of
/// curvature, it runs inside the stroke along `line`; those parts are cut away.
fn offset_polyline(line: &[Point], distance: f32) -> Vec<Point> {
    let normal = |a: Point, b: Point| {
        let d = (b - a).normalize();
        Vector::new(-d.y, d.x) * distance
    };
    let mut raw = Vec::with_capacity(line.len() + 8);
    raw.push(line[0] + normal(line[0], line[1]));
    for w in line.windows(3) {
        let (before, vertex, after) = (w[0], w[1], w[2]);
        let (da, db) = (vertex - before, after - vertex);
        let a = vertex + normal(before, vertex);
        let b = vertex + normal(vertex, after);
        let cross = da.cross(db);
        let joint = if cross.abs() <= 1e-6 * da.length() * db.length() {
            Some(b)
        } else if cross * distance < 0. {
            miter(vertex, a, da, b, db, distance.abs())
        } else {
            // On the inside of the turn, where the two offset segments cross.
            let t = (b - a).cross(db) / cross;
            let u = (b - a).cross(da) / cross;
            ((-1. ..=0.).contains(&t) && (0. ..=1.).contains(&u)).then(|| a + da * t)
        };
        match joint {
            Some(joint) => raw.push(joint),
            None => raw.extend([a, b]),
        }
    }
    let n = line.len();
    raw.push(line[n - 1] + normal(line[n - 2], line[n - 1]));
    uncovered_parts(&raw, &Stroke::new(line, distance.abs()))
}

/// The parts of the polyline `raw` outside `stroke`, joined in order.
fn uncovered_parts(raw: &[Point], stroke: &Stroke) -> Vec<Point> {
    // Loose enough for the stroke's own outline to count as outside.
    let margin = stroke.distance * 1e-5 + 1e-4;
    let outside = |p: Point| !stroke.covers(p, margin);
    // Samples are no further apart than this, so a loop can't slip between two of them.
    let spacing = (stroke.distance / 4.).max(TOLERANCE);
    let mut out: Vec<Point> = Vec::with_capacity(raw.len());
    let push = |out: &mut Vec<Point>, p: Point| {
        if out.last().is_none_or(|last| (p - *last).square_length() > 1e-8) {
            out.push(p);
        }
    };
    // The raw segment along which the polyline last went into the stroke.
    let mut entered: Option<(Point, Point)> = None;
    for w in raw.windows(2) {
        let (a, b) = (w[0], w[1]);
        let samples = ((b - a).length() / spacing).ceil().max(1.) as u32;
        let at = |t: f32| a.lerp(b, t);
        let mut previous = (0., outside(a));
        if previous.1 {
            push(&mut out, a);
        }
        for i in 1..=samples {
            let t = i as f32 / samples as f32;
            let is_outside = outside(at(t));
            if is_outside != previous.1 {
                // Bisect for where the polyline crosses the stroke's outline.
                let (mut lo, mut hi) = (previous.0, t);
                for _ in 0..24 {
                    let mid = (lo + hi) / 2.;
                    if outside(at(mid)) == previous.1 { lo = mid } else { hi = mid }
                }
                let crossing = at(if is_outside { hi } else { lo });
                match entered.take() {
                    None if !is_outside => {
                        entered = Some((a, b));
                        push(&mut out, crossing);
                    }
                    // Leaving where the polyline went in: the true corner is where the two
                    // segments meet, which the bisection only approaches.
                    Some((ea, eb))
                        if out.last().is_some_and(|last| {
                            line_intersection(ea, eb - ea, a, b - a).is_some_and(|x| {
                                (x - *last).length() < stroke.distance
                                    && (x - crossing).length() < stroke.distance
                            })
                        }) =>
                    {
                        let x = line_intersection(ea, eb - ea, a, b - a).unwrap();
                        *out.last_mut().unwrap() = x;
                    }
                    _ => push(&mut out, crossing),
                }
            }
            previous = (t, is_outside);
        }
        if previous.1 {
            push(&mut out, b);
        }
    }
    out
}

/// Where the lines through `a` along `da` and through `b` along `db` meet, unless they're
/// parallel.
fn line_intersection(a: Point, da: Vector, b: Point, db: Vector) -> Option<Point> {
    let cross = da.cross(db);
    (cross.abs() > 1e-6 * da.length() * db.length()).then(|| a + da * ((b - a).cross(db) / cross))
}

/// Where the segments `a0 a1` and `b0 b1` properly cross, if they do.
fn segment_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let (da, db) = (a1 - a0, b1 - b0);
    let denom = da.cross(db);
    if denom.abs() <= f32::EPSILON * da.length() * db.length() {
        return None;
    }
    let t = (b0 - a0).cross(db) / denom;
    let u = (b0 - a0).cross(da) / denom;
    ((0. ..=1.).contains(&t) && (0. ..1.).contains(&u)).then(|| a0 + da * t)
}

/// Drops the leading points on the line `y == line` and the trailing ones on `x == line`,
/// but the last of each, so the edge starts and ends where it leaves the straight edges.
fn trim_to_corner(points: &mut Vec<Point>, line: f32) {
    const EPSILON: f32 = 1e-3;
    let start =
        points.windows(2).position(|w| (w[1].y - line).abs() > EPSILON).unwrap_or(points.len() - 1);
    let end = points
        .windows(2)
        .rposition(|w| (w[0].x - line).abs() > EPSILON)
        .map_or(0, |i| i + 1)
        .max(start);
    points.truncate(end + 1);
    points.drain(..start);
}

/// The two edges of a corner's border, as polylines in the corner frame.
///
/// Both run from the incoming straight edge to the outgoing one, `x` never increasing and `y`
/// never decreasing along the way.
/// The outer edge starts on `y == 0` and ends on `x == 0`, the inner one on `y == w` and `x == w`.
#[derive(Debug, Clone, PartialEq)]
pub struct CornerEdges {
    /// The border's outer edge.
    pub outer: Vec<Point>,
    /// The border's inner edge; the same as `outer` without a border.
    pub inner: Vec<Point>,
}

/// The edges of a corner of `shape` whose border of width `border_width` is centered on a
/// corner of radius `centerline_radius`.
pub fn corner_edges(shape: CornerShape, centerline_radius: f32, border_width: f32) -> CornerEdges {
    let h = border_width.max(0.) / 2.;
    // The error between a miter and the true offset of a curve, `h * turn^2 / 8`, stays
    // within the tolerance.
    let max_turn = if h > 0. { (8. * TOLERANCE / h).sqrt().min(MAX_TURN) } else { MAX_TURN };
    // Straight runs either side of the corner, long enough that no join reaches their end.
    let run = 2. * border_width + 1.;
    let r = centerline_radius.max(0.);

    let mut corner = Vec::new();
    flatten_corner(shape, r, max_turn, &mut corner);
    let mut centerline = Vec::with_capacity(corner.len() + 2);
    centerline.push(Point::new(h + r + run, h));
    for p in corner {
        let p = p + Vector::new(h, h);
        if centerline.last().is_none_or(|last: &Point| (p - *last).square_length() > 1e-8) {
            centerline.push(p);
        }
    }
    centerline.push(Point::new(h, h + r + run));

    let (mut outer, mut inner) = if h > 0. {
        (offset_polyline(&centerline, h), offset_polyline(&centerline, -h))
    } else {
        (centerline.clone(), centerline)
    };
    trim_to_corner(&mut outer, 0.);
    trim_to_corner(&mut inner, 2. * h);
    CornerEdges { outer, inner }
}

/// The corners of a rectangle in path order: tip, incoming edge direction, outgoing edge
/// direction.
pub(crate) fn corner_frames(rect: euclid::default::Rect<f32>) -> [(Point, Vector, Vector); 4] {
    let (x0, y0, x1, y1) = (rect.min_x(), rect.min_y(), rect.max_x(), rect.max_y());
    [
        (Point::new(x1, y0), Vector::new(-1., 0.), Vector::new(0., 1.)),
        (Point::new(x1, y1), Vector::new(0., -1.), Vector::new(-1., 0.)),
        (Point::new(x0, y1), Vector::new(1., 0.), Vector::new(0., -1.)),
        (Point::new(x0, y0), Vector::new(0., 1.), Vector::new(1., 0.)),
    ]
}

/// A rectangle's radii and shapes in the order of [`corner_frames`].
pub(crate) fn corners_in_path_order<U>(
    radius: BorderRadius<f32, U>,
    shapes: CornerShapes,
) -> [(f32, CornerShape); 4] {
    [
        (radius.top_right, shapes.top_right),
        (radius.bottom_right, shapes.bottom_right),
        (radius.bottom_left, shapes.bottom_left),
        (radius.top_left, shapes.top_left),
    ]
}

/// The outer and inner edges of a rectangle's border, as closed polygons in the same order
/// as a rounded rectangle path.
#[derive(Debug, Clone)]
pub struct BorderContours {
    /// The border's outer edge.
    pub outer: Vec<Point>,
    /// The border's inner edge, or `None` when the border covers the whole rectangle.
    pub inner: Option<Vec<Point>>,
}

/// The edges of the border of width `border_width` stroked along the rectangle `rect` with
/// corner radii `radius` and shapes `shapes`.
/// See [`BorderRectLayout`](crate::item_rendering::BorderRectLayout) for how these relate
/// to the rectangle's own geometry.
pub fn border_contours<U>(
    rect: euclid::default::Rect<f32>,
    radius: BorderRadius<f32, U>,
    shapes: CornerShapes,
    border_width: f32,
) -> BorderContours {
    let border_width = border_width.max(0.);
    let h = border_width / 2.;
    let radius = radius.max(BorderRadius::zero()).fit_to_size(rect.width(), rect.height());
    let outer_rect = rect.inflate(h, h);
    let edges = corners_in_path_order(radius, shapes).map(|(r, shape)| {
        let e = corner_edges(shape, r, border_width);
        (e.outer, e.inner)
    });
    let frames = corner_frames(outer_rect);
    let to_world = |polylines: [Vec<Point>; 4]| {
        let mut frames = frames.iter();
        polylines.map(|points| {
            let &(tip, e_in, e_out) = frames.next().unwrap();
            points.into_iter().map(|p| tip + e_in * p.x + e_out * p.y).collect::<Vec<_>>()
        })
    };
    let sides = frames.map(|(_, _, e_out)| e_out);
    let [o0, o1, o2, o3] = edges.clone().map(|e| e.0);
    let [i0, i1, i2, i3] = edges.map(|e| e.1);
    let outer = join_corners(to_world([o0, o1, o2, o3]), sides).unwrap_or_default();
    let inner = (rect.width() > border_width && rect.height() > border_width)
        .then(|| join_corners(to_world([i0, i1, i2, i3]), sides))
        .flatten();
    BorderContours { outer, inner }
}

/// Concatenates the corners of a polygon, cutting adjacent corners at their crossing where
/// they reach past each other along the side between them.
/// `sides[i]` is the direction of the side from corner `i` to the next.
/// Returns `None` when the corners leave no area.
fn join_corners(mut corners: [Vec<Point>; 4], sides: [Vector; 4]) -> Option<Vec<Point>> {
    for (i, side) in sides.into_iter().enumerate() {
        let next = (i + 1) % 4;
        let (Some(&end), Some(&start)) = (corners[i].last(), corners[next].first()) else {
            return None;
        };
        if (start - end).dot(side) >= 0. {
            continue;
        }
        let (a, b) = (&corners[i], &corners[next]);
        let (ja, jb, crossing) = (0..a.len().saturating_sub(1)).rev().find_map(|ja| {
            (0..b.len().saturating_sub(1)).find_map(|jb| {
                segment_intersection(a[ja], a[ja + 1], b[jb], b[jb + 1]).map(|p| (ja, jb, p))
            })
        })?;
        corners[i].truncate(ja + 1);
        corners[i].push(crossing);
        corners[next].drain(..=jb);
        corners[next].insert(0, crossing);
    }
    let mut polygon: Vec<Point> = Vec::new();
    for p in corners.into_iter().flatten() {
        if polygon.last().is_none_or(|last| (p - *last).square_length() > 1e-8) {
            polygon.push(p);
        }
    }
    Some(polygon)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: [CornerShape; 15] = [
        CornerShape::Square,
        CornerShape::Notch,
        CornerShape::Bevel,
        CornerShape::Round,
        CornerShape::Scoop,
        CornerShape::Squircle,
        CornerShape::Superellipse(-6.),
        CornerShape::Superellipse(-2.),
        CornerShape::Superellipse(-0.5),
        CornerShape::Superellipse(0.),
        CornerShape::Superellipse(0.5),
        CornerShape::Superellipse(3.),
        CornerShape::Superellipse(8.),
        CornerShape::Superellipse(f32::INFINITY),
        CornerShape::Superellipse(f32::NAN),
    ];

    fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
        let d = b - a;
        let t = if d.square_length() > 0. {
            ((p - a).dot(d) / d.square_length()).clamp(0., 1.)
        } else {
            0.
        };
        (p - (a + d * t)).length()
    }

    fn distance_to_polyline(p: Point, line: &[Point]) -> f32 {
        if line.len() == 1 {
            return (p - line[0]).length();
        }
        line.windows(2).map(|w| distance_to_segment(p, w[0], w[1])).fold(f32::INFINITY, f32::min)
    }

    /// The centerline `corner_edges` offsets, extended along the straight edges.
    fn centerline(shape: CornerShape, r: f32, w: f32) -> Vec<Point> {
        let h = w / 2.;
        let mut corner = Vec::new();
        flatten_corner(shape, r, 0.01, &mut corner);
        core::iter::once(Point::new(h + r + 4. * w + 10., h))
            .chain(corner.into_iter().map(|p| p + Vector::new(h, h)))
            .chain(core::iter::once(Point::new(h, h + r + 4. * w + 10.)))
            .collect()
    }

    fn assert_monotone(shape: CornerShape, edge: &[Point]) {
        for w in edge.windows(2) {
            assert!(
                w[1].x <= w[0].x + 1e-3 && w[1].y >= w[0].y - 1e-3,
                "{shape:?}: {edge:?} isn't monotone at {:?} -> {:?}",
                w[0],
                w[1]
            );
        }
    }

    /// Every flattened vertex lies on the curve, checked against the curve's own equation.
    #[test]
    fn flatten_corner_lies_on_the_curve() {
        for shape in SHAPES {
            for r in [3f32, 40., 300.] {
                let mut points = Vec::new();
                flatten_corner(shape, r, MAX_TURN, &mut points);
                for p in &points {
                    let (x, y) = (p.x / r, p.y / r);
                    let error = match shape {
                        CornerShape::Square => p.to_vector().length() / r,
                        CornerShape::Notch => distance_to_polyline(
                            Point::new(x, y),
                            &[Point::new(1., 0.), Point::new(1., 1.), Point::new(0., 1.)],
                        ),
                        CornerShape::Bevel => (x + y - 1.).abs(),
                        CornerShape::Round => ((1. - x).hypot(1. - y) - 1.).abs(),
                        CornerShape::Scoop => (x.hypot(y) - 1.).abs(),
                        _ => {
                            let k = superellipse_k(shape).unwrap();
                            let n = superellipse_n(k);
                            let (u, v) = if k < 0. { (x, y) } else { (1. - x, 1. - y) };
                            (u.max(0.).powf(n) + v.max(0.).powf(n)).powf(1. / n) - 1.
                        }
                    };
                    assert!(error.abs() < 1e-3, "{shape:?} r={r}: {p:?} is {error} off");
                }
            }
        }
    }

    /// `Scoop` and `Round` are `superellipse(-1)` and `superellipse(1)`, and a NaN `k` is round.
    #[test]
    fn named_shapes_match_their_superellipse() {
        let deviation = |a: CornerShape, b: CornerShape| {
            let (mut pa, mut pb) = (Vec::new(), Vec::new());
            flatten_corner(a, 100., MAX_TURN, &mut pa);
            flatten_corner(b, 100., MAX_TURN, &mut pb);
            pa.iter().map(|p| distance_to_polyline(*p, &pb)).fold(0f32, f32::max)
        };
        for (named, k) in [
            (CornerShape::Round, 1.),
            (CornerShape::Scoop, -1.),
            (CornerShape::Squircle, 2.),
            (CornerShape::Round, f32::NAN),
        ] {
            let d = deviation(named, CornerShape::Superellipse(k));
            assert!(d < 0.05, "{named:?} vs superellipse({k}): {d}");
        }
    }

    /// A round corner's edges are the circles of radius `r + w/2` and `r - w/2` around the
    /// centerline's center.
    #[test]
    fn round_edges_are_concentric_circles() {
        for (r, w) in [(20f32, 4f32), (6., 10.), (100., 1.)] {
            let edges = corner_edges(CornerShape::Round, r, w);
            let h = w / 2.;
            let center = Point::new(h + r, h + r);
            for (name, edge, want) in
                [("outer", &edges.outer, r + h), ("inner", &edges.inner, r - h)]
            {
                for p in edge {
                    let d = (*p - center).length();
                    assert!((d - want).abs() < 0.03, "r={r} w={w}: {name} {p:?} at {d}");
                }
            }
        }
        // Past the radius, the inner straight edges meet in a sharp corner.
        let edges = corner_edges(CornerShape::Round, 3., 10.);
        assert_eq!(edges.inner.len(), 1);
        assert!((edges.inner[0] - Point::new(10., 10.)).length() < 1e-3, "{:?}", edges.inner);
    }

    /// The bevel's diagonal offset by `w/2` either way, and the notch's miters, from their
    /// closed forms.
    #[test]
    fn straight_shapes_match_their_closed_form() {
        let (r, w) = (20f32, 6f32);
        let h = w / 2.;
        let bevel = corner_edges(CornerShape::Bevel, r, w);
        let diagonal = h + r + h;
        let offset = h * core::f32::consts::SQRT_2;
        assert_eq!(bevel.outer.len(), 2);
        assert_eq!(bevel.inner.len(), 2);
        for p in &bevel.outer {
            assert!((p.x + p.y - (diagonal - offset)).abs() < 1e-3, "outer {p:?}");
        }
        for p in &bevel.inner {
            assert!((p.x + p.y - (diagonal + offset)).abs() < 1e-3, "inner {p:?}");
        }
        assert!((bevel.inner[0].y - w).abs() < 1e-3 && (bevel.inner[1].x - w).abs() < 1e-3);

        let notch = corner_edges(CornerShape::Notch, r, w);
        let close = |a: &[Point], b: &[Point]| {
            a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (*p - *q).length() < 1e-3)
        };
        let wall = h + r;
        let expected_outer =
            [Point::new(wall - h, 0.), Point::new(wall - h, wall - h), Point::new(0., wall - h)];
        let expected_inner =
            [Point::new(wall + h, w), Point::new(wall + h, wall + h), Point::new(w, wall + h)];
        assert!(close(&notch.outer, &expected_outer), "outer {:?}", notch.outer);
        assert!(close(&notch.inner, &expected_inner), "inner {:?}", notch.inner);

        let square = corner_edges(CornerShape::Square, r, w);
        assert!(close(&square.outer, &[Point::new(0., 0.)]), "outer {:?}", square.outer);
        assert!(close(&square.inner, &[Point::new(w, w)]), "inner {:?}", square.inner);
    }

    /// No edge point comes closer to the centerline than half the border, however thick the
    /// border is against the curve: the offset's loops are all cut away.
    #[test]
    fn edges_stay_half_a_border_from_the_centerline() {
        for shape in SHAPES {
            for (r, w) in [(30f32, 2f32), (30., 20.), (8., 30.), (100., 90.)] {
                let edges = corner_edges(shape, r, w);
                let center = centerline(shape, r, w);
                for (name, edge) in [("outer", &edges.outer), ("inner", &edges.inner)] {
                    assert_monotone(shape, edge);
                    for p in edge {
                        let d = distance_to_polyline(*p, &center);
                        assert!(
                            d > w / 2. - 0.05,
                            "{shape:?} r={r} w={w}: {name} point {p:?} is {d} from the centerline"
                        );
                    }
                }
            }
        }
    }

    /// Every centerline point is half a border from both edges.
    #[test]
    fn edges_follow_the_centerline() {
        // Where the curve bends tighter than half the border, the edge rounds it off and
        // stays further away; these shapes don't.
        for shape in [
            CornerShape::Round,
            CornerShape::Scoop,
            CornerShape::Squircle,
            CornerShape::Superellipse(0.5),
        ] {
            for (r, w) in [(40f32, 4f32), (80., 10.)] {
                let edges = corner_edges(shape, r, w);
                let mut corner = Vec::new();
                flatten_corner(shape, r, 0.01, &mut corner);
                // A concave corner meets the straight edges at an angle, where the inner
                // edge is trimmed further away.
                let (start, end) = (corner[0], *corner.last().unwrap());
                let away_from_ends =
                    |p: &Point| (*p - start).length() > w && (*p - end).length() > w;
                for p in corner
                    .iter()
                    .filter(|p| away_from_ends(p))
                    .map(|p| *p + Vector::new(w / 2., w / 2.))
                {
                    for (name, edge) in [("outer", &edges.outer), ("inner", &edges.inner)] {
                        let d = distance_to_polyline(p, edge);
                        assert!(
                            (d - w / 2.).abs() < 0.05,
                            "{shape:?} r={r} w={w}: {name} edge is {d} from {p:?}"
                        );
                    }
                }
            }
        }
    }

    fn polygon_is_simple(polygon: &[Point]) -> bool {
        let n = polygon.len();
        (0..n).all(|i| {
            (i + 2..n).all(|j| {
                if i == 0 && j == n - 1 {
                    return true;
                }
                let (a0, a1) = (polygon[i], polygon[(i + 1) % n]);
                let (b0, b1) = (polygon[j], polygon[(j + 1) % n]);
                segment_intersection(a0, a1, b0, b1).is_none_or(|p| {
                    // Touching at a shared vertex isn't a crossing.
                    [a0, a1].iter().any(|v| (*v - p).length() < 1e-3)
                        && [b0, b1].iter().any(|v| (*v - p).length() < 1e-3)
                })
            })
        })
    }

    /// Adjacent corners whose inner edges reach past each other are cut where they cross,
    /// leaving a simple polygon.
    #[test]
    fn border_contours_join_overlapping_corners() {
        let square = euclid::default::Rect::new(Point::new(5., 5.), euclid::size2(40., 40.));
        // Notches overlapping along a side leave no area in the rows they cover.
        let tall = euclid::default::Rect::new(Point::new(5., 5.), euclid::size2(40., 80.));
        for (shape, rect) in
            [(CornerShape::Bevel, square), (CornerShape::Scoop, square), (CornerShape::Notch, tall)]
        {
            let contours = border_contours::<()>(
                rect,
                BorderRadius::new_uniform(20.),
                CornerShapes::new_uniform(shape),
                10.,
            );
            let inner = contours.inner.unwrap_or_else(|| panic!("{shape:?}: no inner region"));
            assert!(polygon_is_simple(&inner), "{shape:?}: {inner:?}");
            assert!(polygon_is_simple(&contours.outer), "{shape:?}: {:?}", contours.outer);
            let inner_rect = rect.inflate(-5., -5.);
            for p in &inner {
                assert!(
                    p.x >= inner_rect.min_x() - 1e-3
                        && p.x <= inner_rect.max_x() + 1e-3
                        && p.y >= inner_rect.min_y() - 1e-3
                        && p.y <= inner_rect.max_y() + 1e-3,
                    "{shape:?}: {p:?} lies outside the inner rectangle"
                );
            }
        }
        let notched = border_contours::<()>(
            square,
            BorderRadius::new_uniform(20.),
            CornerShapes::new_uniform(CornerShape::Notch),
            10.,
        );
        assert!(notched.inner.is_none(), "{:?}", notched.inner);
        let covered = border_contours::<()>(
            square,
            BorderRadius::new_uniform(4.),
            CornerShapes::new_uniform(CornerShape::Bevel),
            40.,
        );
        assert!(covered.inner.is_none());
    }
}
