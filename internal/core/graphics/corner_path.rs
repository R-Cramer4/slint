// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
Builds an exact path for a rectangle whose corners are cut according to a
[`CornerShape`](crate::items::CornerShape) per corner, for renderers (Skia, FemtoVG) that
draw paths rather than rasterize scanlines directly. Callers with only circular corners
should keep using their native rounded-rect primitive instead; this is for the shapes
that primitive can't express.
*/

use super::border_radius::{corner_boundary, superellipse_n_from_k, SQUIRCLE_EXPONENT};
use super::BorderRadius;
use crate::items::CornerShape;
use crate::lengths::CornerShapes;
use lyon_path::math::{Point, Vector};
use lyon_path::traits::SvgPathBuilder;

/// The standard cubic-bezier approximation factor for a quarter circle.
const KAPPA: f32 = 0.552_284_75;

/// Number of `line_to` segments used to approximate a superellipse-family corner
/// (`Squircle`, `Superellipse`). Unlike the circular arcs above, a general superellipse
/// has no closed-form Bezier fit, so it's tessellated instead; scale the segment count
/// with `r` (in physical pixels) so large corners don't show visible facets.
fn superellipse_steps(r: f32) -> u32 {
    (r * 0.75).clamp(8.0, 128.0) as u32
}

/// Draws the corner at `tip`, from the current position (`tip + r * e_in`) to
/// `tip + r * e_out`, where `e_in`/`e_out` are unit vectors from the tip along the
/// incoming/outgoing straight edges. The caller has already moved to `tip + r * e_in`.
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
        CornerShape::Round => {
            b.cubic_bezier_to(
                tip + e_in * r + e_out * (r - r * KAPPA),
                tip + e_in * (r - r * KAPPA) + e_out * r,
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
        CornerShape::Squircle => {
            let steps = superellipse_steps(r);
            for i in 1..=steps {
                let t = r * (i as f32) / (steps as f32);
                let x = corner_boundary(shape, SQUIRCLE_EXPONENT, r, t);
                b.line_to(tip + e_in * x + e_out * t);
            }
        }
        CornerShape::Superellipse(k) => {
            let steps = superellipse_steps(r);
            let n = superellipse_n_from_k(k);
            for i in 1..=steps {
                let t = r * (i as f32) / (steps as f32);
                let x = corner_boundary(shape, n, r, t);
                b.line_to(tip + e_in * x + e_out * t);
            }
        }
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
