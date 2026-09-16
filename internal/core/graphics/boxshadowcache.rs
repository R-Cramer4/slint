// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
This module contains a cache helper for caching box shadow textures.
*/

use alloc::boxed::Box;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};

use crate::items::ItemRc;
use crate::{
    Color,
    lengths::{CornerShapes, PhysicalBorderRadius, PhysicalPx, RectLengths, ScaleFactor},
};

/// Struct to store options affecting the rendering of a box shadow
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BoxShadowOptions {
    /// The width of the box shadow texture in physical pixels.
    pub width: euclid::Length<f32, PhysicalPx>,
    /// The height of the box shadow texture in physical pixels.
    pub height: euclid::Length<f32, PhysicalPx>,
    /// The color for the box shadow.
    pub color: Color,
    /// The blur to apply to the box shadow in pixels.
    pub blur: euclid::Length<f32, PhysicalPx>,
    /// The radii of the box shadow.
    pub radius: PhysicalBorderRadius,
    /// The shape of each corner of the box shadow.
    pub corner_shape: CornerShapes,
    /// The spread radius in physical pixels. Positive grows the shadow shape, negative shrinks it.
    pub spread: euclid::Length<f32, PhysicalPx>,
    /// Whether the shadow is rendered inside the element's geometry.
    pub inset: bool,
    /// Horizontal offset in physical pixels. Only used by inset shadows (drop-shadow offset is
    /// applied at blit time and is not part of the cached image).
    pub offset_x_inset: f32,
    /// Vertical offset in physical pixels. Only used by inset shadows.
    pub offset_y_inset: f32,
}

impl Eq for BoxShadowOptions {}
impl Ord for BoxShadowOptions {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Two tuples rather than one: std only implements `PartialOrd` for tuples up to 12 elements.
        let lhs = (
            self.width,
            self.height,
            self.color,
            self.blur,
            self.radius.top_left.to_bits(),
            self.radius.top_right.to_bits(),
            self.radius.bottom_right.to_bits(),
            self.radius.bottom_left.to_bits(),
            self.spread,
            self.inset,
            self.offset_x_inset.to_bits(),
            self.offset_y_inset.to_bits(),
        );
        let rhs = (
            other.width,
            other.height,
            other.color,
            other.blur,
            other.radius.top_left.to_bits(),
            other.radius.top_right.to_bits(),
            other.radius.bottom_right.to_bits(),
            other.radius.bottom_left.to_bits(),
            other.spread,
            other.inset,
            other.offset_x_inset.to_bits(),
            other.offset_y_inset.to_bits(),
        );
        // Order by variant index and, for `Superellipse`, its exponent bits.
        fn corner_shape_key(shape: super::border_radius::CornerShape) -> (u32, u32) {
            use super::border_radius::CornerShape;
            match shape {
                CornerShape::Round => (0, 0),
                CornerShape::Squircle => (1, 0),
                CornerShape::Bevel => (2, 0),
                CornerShape::Scoop => (3, 0),
                CornerShape::Notch => (4, 0),
                CornerShape::Square => (5, 0),
                CornerShape::Superellipse(k) => (6, k.to_bits()),
            }
        }
        let lhs_shape = (
            corner_shape_key(self.corner_shape.top_left),
            corner_shape_key(self.corner_shape.top_right),
            corner_shape_key(self.corner_shape.bottom_right),
            corner_shape_key(self.corner_shape.bottom_left),
        );
        let rhs_shape = (
            corner_shape_key(other.corner_shape.top_left),
            corner_shape_key(other.corner_shape.top_right),
            corner_shape_key(other.corner_shape.bottom_right),
            corner_shape_key(other.corner_shape.bottom_left),
        );
        if rhs < lhs {
            std::cmp::Ordering::Less
        } else if lhs < rhs {
            std::cmp::Ordering::Greater
        } else if rhs_shape < lhs_shape {
            std::cmp::Ordering::Less
        } else if lhs_shape < rhs_shape {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }
}

impl PartialOrd for BoxShadowOptions {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The geometry of a box shadow, derived from [`BoxShadowOptions`].
///
/// The CSS rules the renderers agree on: the shadow shape is the element's geometry
/// grown by the spread, its corner radii grow with it, and the texture it is rendered
/// into is padded by the blur on every side so the Gaussian can fade out to
/// transparency within it.
impl BoxShadowOptions {
    /// The size of the shadow shape: the element's size grown by the spread on each
    /// side. A negative spread shrinks it, down to nothing.
    pub fn shape_size(&self) -> euclid::Size2D<f32, PhysicalPx> {
        euclid::size2(
            (self.width.get() + 2. * self.spread.get()).max(0.),
            (self.height.get() + 2. * self.spread.get()).max(0.),
        )
    }

    /// The size of the texture a drop shadow is rendered into: the shape padded by the
    /// blur on each side.
    pub fn drop_texture_size(&self) -> euclid::Size2D<f32, PhysicalPx> {
        self.shape_size() + euclid::size2(2. * self.blur.get(), 2. * self.blur.get())
    }

    /// Where the shape sits within the drop shadow texture, i.e. the blur padding.
    pub fn shape_origin(&self) -> euclid::Point2D<f32, PhysicalPx> {
        euclid::point2(self.blur.get(), self.blur.get())
    }

    /// The corner radii of the shadow shape: `max(0, radius + spread * scale)`, `scale`
    /// from [`CornerShape::spread_scale`].
    pub fn outer_radius(&self) -> PhysicalBorderRadius {
        self.offset_radius(self.spread.get())
    }

    /// The corner radii of the hole an inset shadow leaves: [`Self::outer_radius`] with the
    /// spread negated.
    pub fn inner_radius(&self) -> PhysicalBorderRadius {
        self.offset_radius(-self.spread.get())
    }

    /// `radius` moved outward by `delta` per corner, scaled by [`CornerShape::spread_scale`]
    /// and clamped to zero. A corner with no radius stays sharp, per CSS's `box-shadow`
    /// spread.
    fn offset_radius(&self, delta: f32) -> PhysicalBorderRadius {
        let corner = |r: f32, shape: super::border_radius::CornerShape| {
            if r > 0. { (r + delta * shape.spread_scale()).max(0.) } else { 0. }
        };
        PhysicalBorderRadius::new(
            corner(self.radius.top_left, self.corner_shape.top_left),
            corner(self.radius.top_right, self.corner_shape.top_right),
            corner(self.radius.bottom_right, self.corner_shape.bottom_right),
            corner(self.radius.bottom_left, self.corner_shape.bottom_left),
        )
    }

    /// The Gaussian sigma corresponding to the CSS blur radius.
    pub fn blur_sigma(&self) -> f32 {
        self.blur.get() / 2.
    }

    /// The rectangle of the hole an inset shadow's shape leaves in
    /// [`Self::inset_shadow_ring_path`]: the geometry inset by `spread` on each side,
    /// translated by the inset offset.
    #[cfg(feature = "path")]
    pub fn inset_hole_rect(&self) -> euclid::Rect<f32, PhysicalPx> {
        let spread = self.spread.get();
        euclid::Rect::new(
            euclid::Point2D::new(spread + self.offset_x_inset, spread + self.offset_y_inset),
            euclid::Size2D::new(
                (self.width.get() - 2. * spread).max(0.),
                (self.height.get() - 2. * spread).max(0.),
            ),
        )
    }

    /// The path an inset shadow paints, before blur and clipping to the element's shape: an
    /// outer rectangle inflated well past the geometry, so its edge stays hidden after
    /// blurring, with [`Self::inset_hole_rect`] cut out of it.
    /// Fill with the even-odd rule; the renderer then blurs it and clips to `width` x
    /// `height` with `radius`/`corner_shape`.
    #[cfg(feature = "path")]
    pub fn inset_shadow_ring_path(&self) -> lyon_path::Path {
        let width = self.width.get();
        let height = self.height.get();
        let spread = self.spread.get();
        // Far enough out that the outer edge stays outside the geometry even after the blur,
        // an offset hole, or a negative spread.
        let inflate = self.blur.get()
            + spread.abs()
            + self.offset_x_inset.abs()
            + self.offset_y_inset.abs()
            + 16.;

        let mut outer = lyon_path::Path::builder().with_svg();
        outer.move_to(lyon_path::math::point(-inflate, -inflate));
        outer.line_to(lyon_path::math::point(width + inflate, -inflate));
        outer.line_to(lyon_path::math::point(width + inflate, height + inflate));
        outer.line_to(lyon_path::math::point(-inflate, height + inflate));
        outer.close();
        let outer = outer.build();

        // The hole shrinks by `spread`, mirroring how a drop shadow's shape grows: negative-spread
        // `spread_rounded_rect_path` is the exact offset curve, unlike `rounded_rect_path` at
        // `inner_radius`'s scaled radius.
        let hole = self.inset_hole_rect();
        let hole = super::corner_path::spread_rounded_rect_path(
            hole.origin.x,
            hole.origin.y,
            hole.size.width,
            hole.size.height,
            self.radius,
            self.corner_shape,
            -spread,
        );

        let mut builder = lyon_path::Path::builder();
        builder.extend_from_paths(&[outer.as_slice(), hole.as_slice()]);
        builder.build()
    }

    /// Extracts the rendering specific properties from the BoxShadow item and scales the logical
    /// coordinates to physical pixels used in the BoxShadowOptions. Returns None if for example the
    /// alpha on the box shadow would imply that no shadow is to be rendered.
    pub fn new(
        item_rc: &ItemRc,
        box_shadow: std::pin::Pin<&crate::items::BoxShadow>,
        scale_factor: ScaleFactor,
    ) -> Option<Self> {
        let color = box_shadow.color();
        if color.alpha() == 0 {
            return None;
        }
        let geometry = item_rc.geometry();
        let width = geometry.width_length() * scale_factor;
        let height = geometry.height_length() * scale_factor;
        if width.get() < 1. || height.get() < 1. {
            return None;
        }
        let inset = box_shadow.inset();
        let (offset_x_inset, offset_y_inset) = if inset {
            (
                (box_shadow.offset_x() * scale_factor).get(),
                (box_shadow.offset_y() * scale_factor).get(),
            )
        } else {
            (0., 0.)
        };
        Some(Self {
            width,
            height,
            color,
            blur: box_shadow.blur() * scale_factor, // This effectively becomes the blur radius, so scale to physical pixels
            radius: box_shadow.logical_border_radius() * scale_factor,
            corner_shape: box_shadow.logical_corner_shape(),
            spread: box_shadow.spread() * scale_factor,
            inset,
            offset_x_inset,
            offset_y_inset,
        })
    }
}

/// Upper bound on the number of shadow textures kept alive by a [`BoxShadowCache`].
const MAX_CACHED_SHADOWS: usize = 16;

struct CacheEntry<ImageType> {
    image: Option<ImageType>,
    /// Value of the cache's access counter when this entry was last returned, for LRU eviction.
    last_used: u64,
}

/// Cache to hold box textures for given box shadow options.
pub struct BoxShadowCache<ImageType> {
    entries: RefCell<BTreeMap<BoxShadowOptions, CacheEntry<ImageType>>>,
    access_counter: Cell<u64>,
    /// Track if the window scale factor changes; used to clear the cache if necessary.
    window_scale_factor_tracker: core::pin::Pin<Box<crate::properties::PropertyTracker>>,
}

impl<ImageType> Default for BoxShadowCache<ImageType> {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            access_counter: Default::default(),
            window_scale_factor_tracker: Box::pin(Default::default()),
        }
    }
}

impl<ImageType> BoxShadowCache<ImageType> {
    /// Removes all cached box shadow textures.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    /// Clears the cache if the window's scale factor has changed since the last call, as the
    /// cached textures are rendered in physical pixels.
    pub fn clear_cache_if_scale_factor_changed(&self, window: &crate::api::Window) {
        if self.window_scale_factor_tracker.is_dirty() {
            self.window_scale_factor_tracker
                .as_ref()
                .evaluate_as_dependency_root(|| window.scale_factor());
            self.clear();
        }
    }
}

impl<ImageType: Clone> BoxShadowCache<ImageType> {
    /// Look up a box shadow texture for a given box shadow item, or create a new one if needed.
    pub fn get_box_shadow(
        &self,
        item_rc: &ItemRc,
        item_cache: &crate::item_rendering::ItemCache<Option<ImageType>>,
        box_shadow: std::pin::Pin<&crate::items::BoxShadow>,
        scale_factor: ScaleFactor,
        shadow_render_fn: impl FnOnce(&BoxShadowOptions) -> Option<ImageType>,
    ) -> Option<ImageType> {
        item_cache.get_or_update_cache_entry(item_rc, || {
            let shadow_options = BoxShadowOptions::new(item_rc, box_shadow, scale_factor)?;
            let mut entries = self.entries.borrow_mut();
            // Shadow options that change on every frame (an animated blur for example) would grow
            // the cache without bounds, so evict the least recently used entry when it gets too big.
            // Note that evicted images may still be alive through the per-item cache; eviction only
            // means that the shadow has to be re-rendered on the next per-item cache miss.
            if entries.len() >= MAX_CACHED_SHADOWS
                && !entries.contains_key(&shadow_options)
                && let Some(least_recently_used) = entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(options, _)| options.clone())
            {
                entries.remove(&least_recently_used);
            }
            let stamp = self.access_counter.get() + 1;
            self.access_counter.set(stamp);
            let entry = entries.entry(shadow_options.clone()).or_insert_with(|| CacheEntry {
                image: shadow_render_fn(&shadow_options),
                last_used: stamp,
            });
            entry.last_used = stamp;
            entry.image.clone()
        })
    }
}
