// Copyright © Klarälvdalens Datakonsult AB, a KDAB Group company , info@kdab.com, author Robin Cramer <robin.cramer@kdab.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! The pixel coverage of rectangle corners whose shape isn't round.

use alloc::vec::Vec;
use i_slint_core::graphics::corner_geometry::corner_edges;
use i_slint_core::graphics::{BorderRadius, CornerShapes};
use i_slint_core::item_rendering::border_fill_radius;
use i_slint_core::lengths::PhysicalPx;
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use num_traits::Float;

type Point = euclid::default::Point2D<f32>;

/// A corner of a rectangle, as an index into [`ShapedCorners`].
#[derive(Debug, Clone, Copy)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

/// How much of each pixel in a rectangle's corners lies inside its border's outer edge, and
/// inside its inner edge, from 0 to 255.
///
/// A corner is addressed by row, counting from the corner's horizontal edge, and column,
/// counting from its vertical edge.
/// Corners are symmetric about their diagonal, so which edge is which doesn't matter.
#[derive(Debug)]
pub struct ShapedCorners {
    corners: [CornerCoverage; 4],
    border_width: usize,
}

impl ShapedCorners {
    /// The coverage of the corners with `radius` and `shapes`, under a border of `border_width`.
    pub fn new(
        radius: BorderRadius<f32, PhysicalPx>,
        shapes: CornerShapes,
        border_width: i16,
    ) -> Self {
        let border = euclid::Length::new(border_width as f32);
        let centerline = border_fill_radius(radius, border).inner(border / 2.);
        let corner = |r: f32, shape| {
            let edges = corner_edges(shape, r, border.get());
            CornerCoverage::new(&edges.outer, &edges.inner, border_width as usize)
        };
        Self {
            corners: [
                corner(centerline.top_left, shapes.top_left),
                corner(centerline.top_right, shapes.top_right),
                corner(centerline.bottom_right, shapes.bottom_right),
                corner(centerline.bottom_left, shapes.bottom_left),
            ],
            border_width: border_width as usize,
        }
    }

    pub fn border_width(&self) -> usize {
        self.border_width
    }

    /// The rows of `corner` that its shape affects; past them, the rectangle's sides are
    /// straight.
    pub fn height(&self, corner: Corner) -> usize {
        self.corners[corner as usize].rows.len()
    }

    /// The columns of `row` in `corner` past which the coverage no longer changes.
    pub fn width(&self, corner: Corner, row: usize) -> usize {
        self.corners[corner as usize].rows.get(row).map_or(self.border_width, |r| r.end())
    }

    /// The `(outer, inner)` coverage of the pixel at `row` and `column` of `corner`.
    pub fn coverage(&self, corner: Corner, row: usize, column: usize) -> (u8, u8) {
        let inner_past_border = if column < self.border_width { 0 } else { 255 };
        let Some(r) = self.corners[corner as usize].rows.get(row) else {
            return (255, inner_past_border);
        };
        let first = r.first_column as usize;
        if column < first {
            (0, 0)
        } else if column >= r.end() {
            (255, if row < self.border_width { 0 } else { 255 })
        } else {
            self.corners[corner as usize].coverage[r.start as usize + column - first]
        }
    }
}

#[derive(Debug)]
struct CornerCoverage {
    rows: Vec<CoverageRow>,
    /// The `(outer, inner)` coverage of the pixels the rows list, one row after another.
    coverage: Vec<(u8, u8)>,
}

/// The pixels of a row whose coverage is partial; before them it's empty, after them full.
#[derive(Debug, Clone, Copy)]
struct CoverageRow {
    first_column: u16,
    len: u16,
    /// Where the row's pixels start in [`CornerCoverage::coverage`].
    start: u32,
}

impl CoverageRow {
    fn end(&self) -> usize {
        self.first_column as usize + self.len as usize
    }
}

impl CornerCoverage {
    /// `outer` and `inner` are the border's edges in the corner frame of
    /// [`corner_geometry`](i_slint_core::graphics::corner_geometry).
    fn new(outer: &[Point], inner: &[Point], border_width: usize) -> Self {
        let last_y = |edge: &[Point]| edge.last().map_or(0., |p| p.y);
        let height = last_y(outer).max(last_y(inner)).ceil().max(border_width as f32) as usize;
        let mut rows = Vec::with_capacity(height);
        let mut coverage = Vec::new();
        for row in 0..height {
            let outer_row = RowCoverage::new(outer, row as f32);
            // The fill starts below the border, at the integer row `border_width`.
            let inner_row = (row >= border_width).then(|| RowCoverage::new(inner, row as f32));
            let first =
                inner_row.as_ref().map_or(outer_row.first, |r| r.first.min(outer_row.first));
            let end = inner_row.as_ref().map_or(outer_row.end(), |r| r.end().max(outer_row.end()));
            rows.push(CoverageRow {
                first_column: first as u16,
                len: (end - first) as u16,
                start: coverage.len() as u32,
            });
            coverage.extend((first..end).map(|column| {
                let outer = outer_row.at(column);
                let inner = inner_row.as_ref().map_or(0, |r| r.at(column));
                (outer, inner.min(outer))
            }));
        }
        Self { rows, coverage }
    }
}

/// The share of each pixel in a row that lies right of an edge, for the columns where it's
/// neither 0 nor 1.
struct RowCoverage {
    first: usize,
    area: Vec<f32>,
}

impl RowCoverage {
    /// `edge` is a polyline that never goes up or right, and covers the row: every point to
    /// its right is inside.
    /// Past its last point, it continues straight down.
    fn new(edge: &[Point], row: f32) -> Self {
        let (top, bottom) = (row, row + 1.);
        let end = *edge.last().unwrap();
        let extension = [end, Point::new(end.x, bottom.max(end.y))];
        let pieces: Vec<(Point, Point)> = edge
            .windows(2)
            .map(|w| (w[0], w[1]))
            .chain(core::iter::once((extension[0], extension[1])))
            .filter(|(a, b)| b.y > a.y && b.y > top && a.y < bottom)
            .map(|(a, b)| {
                let x_at = |y: f32| a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y);
                let (y0, y1) = (a.y.max(top), b.y.min(bottom));
                (Point::new(x_at(y0), y0), Point::new(x_at(y1), y1))
            })
            .collect();
        let min_x = pieces.iter().map(|(a, b)| a.x.min(b.x)).fold(f32::INFINITY, f32::min);
        let max_x = pieces.iter().map(|(a, b)| a.x.max(b.x)).fold(0f32, f32::max);
        let first = min_x.max(0.).floor() as usize;
        let end = (max_x.ceil() as usize).max(first);
        let mut area = alloc::vec![0f32; end - first];
        for (a, b) in pieces {
            let height = b.y - a.y;
            for (i, area) in area.iter_mut().enumerate() {
                *area += height * share_right_of(((first + i) as f32, a.x, b.x));
            }
        }
        Self { first, area }
    }

    fn end(&self) -> usize {
        self.first + self.area.len()
    }

    fn at(&self, column: usize) -> u8 {
        if column < self.first {
            0
        } else if column >= self.end() {
            255
        } else {
            (self.area[column - self.first].clamp(0., 1.) * 255.).round() as u8
        }
    }
}

/// The share of the pixel column starting at `column` right of a line running from `x0` to
/// `x1` over the pixel's height.
fn share_right_of((column, x0, x1): (f32, f32, f32)) -> f32 {
    let right = column + 1.;
    let (lo, hi) = (x0.min(x1), x0.max(x1));
    if hi - lo < 1e-6 {
        return (right - lo).clamp(0., 1.);
    }
    // Integrate `clamp(right - x, 0, 1)` over `lo..hi`: 1 left of the column, falling
    // linearly across it, 0 right of it.
    let full = (hi.min(column) - lo).max(0.);
    let (a, b) = (lo.max(column), hi.min(right));
    let partial = if b > a { right * (b - a) - (b * b - a * a) / 2. } else { 0. };
    (full + partial) / (hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::graphics::CornerShape;

    #[test]
    fn share_right_of_a_line() {
        // A vertical line through the middle of the pixel.
        assert_eq!(share_right_of((2., 2.5, 2.5)), 0.5);
        // Left and right of the pixel.
        assert_eq!(share_right_of((2., 1., 1.)), 1.);
        assert_eq!(share_right_of((2., 3.5, 3.5)), 0.);
        // A diagonal across the pixel leaves half of it on either side.
        assert!((share_right_of((2., 2., 3.)) - 0.5).abs() < 1e-6);
        // A diagonal across two pixels leaves a quarter of the left one right of it, and
        // three quarters of the right one.
        assert!((share_right_of((2., 2., 4.)) - 0.25).abs() < 1e-6);
        assert!((share_right_of((3., 2., 4.)) - 0.75).abs() < 1e-6);
    }

    /// The coverage missing from a corner is the area its shape cuts away.
    #[test]
    fn coverage_adds_up_to_the_area() {
        let r = 20f32;
        for (shape, area) in [
            (CornerShape::Square, 0.),
            (CornerShape::Bevel, r * r / 2.),
            (CornerShape::Notch, r * r),
            (CornerShape::Round, r * r * (1. - core::f32::consts::FRAC_PI_4)),
            (CornerShape::Scoop, r * r * core::f32::consts::FRAC_PI_4),
        ] {
            let edges = corner_edges(shape, r, 0.);
            let corner = CornerCoverage::new(&edges.outer, &edges.inner, 0);
            // Sum the outer coverage over the `r` x `r` square at the tip.
            let covered: f32 = (0..r as usize)
                .map(|row| {
                    let row_info = corner.rows.get(row);
                    (0..r as usize)
                        .map(|column| match row_info {
                            Some(info) if column < info.first_column as usize => 0.,
                            Some(info) if column < info.end() => {
                                corner.coverage
                                    [info.start as usize + column - info.first_column as usize]
                                    .0 as f32
                                    / 255.
                            }
                            _ => 1.,
                        })
                        .sum::<f32>()
                })
                .sum();
            let outside = r * r - covered;
            assert!((outside - area).abs() < 0.02 * r * r, "{shape:?}: {outside} vs {area}");
        }
    }

    /// Transposing a corner leaves its coverage unchanged, so a rotated rectangle draws the
    /// same pixels.
    #[test]
    fn coverage_is_symmetric() {
        for shape in
            [CornerShape::Bevel, CornerShape::Notch, CornerShape::Squircle, CornerShape::Scoop]
        {
            let corners = ShapedCorners::new(
                BorderRadius::new_uniform(18.),
                CornerShapes::new_uniform(shape),
                5,
            );
            for row in 0..24 {
                for column in 0..24 {
                    let a = corners.coverage(Corner::TopLeft, row, column);
                    let b = corners.coverage(Corner::TopLeft, column, row);
                    assert!(
                        a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1,
                        "{shape:?} at ({row}, {column}): {a:?} vs {b:?}"
                    );
                }
            }
        }
    }
}
