// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
This module contains the builtin image related items.

When adding an item or a property, it needs to be kept in sync with different place.
Lookup the [`crate::items`] module documentation.
*/
use super::{
    ImageFit, ImageHorizontalAlignment, ImageRendering, ImageTiling, ImageVerticalAlignment, Item,
    ItemConsts, ItemRc, RenderingResult,
};
use crate::animations::Instant;
#[cfg(feature = "animated-images")]
use crate::graphics::ImageInner;
use crate::graphics::{Image, ImageCacheKey};
use crate::input::{
    FocusEvent, FocusEventResult, InputEventFilterResult, InputEventResult, InternalKeyEvent,
    KeyEventResult, MouseEvent,
};
use crate::item_rendering::ItemRenderer;
use crate::item_rendering::{CachedRenderingData, RenderImage};
use crate::item_tree::ItemWeak;
use crate::layout::{LayoutInfo, Orientation};
use crate::lengths::{LogicalLength, LogicalRect, LogicalSize};
use crate::properties::ChangeTracker;
#[cfg(feature = "rtti")]
use crate::rtti::*;
use crate::timers::Timer;
#[cfg(feature = "animated-images")]
use crate::timers::TimerMode;
use crate::window::WindowAdapter;
use crate::{Brush, Coord, Property};
use alloc::boxed::Box;
use alloc::rc::Rc;
use const_field_offset::FieldOffsets;
use core::cell::{Cell, RefCell};
use core::pin::Pin;
use core::time::Duration;
use i_slint_core_macros::*;
use vtable::HasStaticVTable;

/// Opaque box holding the animated-image playback state (current frame, timer,
/// pause bookkeeping) for `ImageItem`/`ClippedImage`. Exposed to C++ as an opaque
/// forward declaration, the same trick used by `FlickableDataBox`/`SystemTrayIconDataBox`
/// (see `internal/core/items/flickable.rs`, `internal/core/items/system_tray.rs`).
#[repr(C)]
pub struct AnimatedPlaybackBox(core::ptr::NonNull<AnimatedPlayback>);

impl Default for AnimatedPlaybackBox {
    fn default() -> Self {
        AnimatedPlaybackBox(Box::leak(Box::<AnimatedPlayback>::default()).into())
    }
}

impl Drop for AnimatedPlaybackBox {
    fn drop(&mut self) {
        // Safety: self.0 was constructed from a Box::leak in AnimatedPlaybackBox::default
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl core::ops::Deref for AnimatedPlaybackBox {
    type Target = AnimatedPlayback;
    fn deref(&self) -> &Self::Target {
        // Safety: initialized in AnimatedPlaybackBox::default
        unsafe { self.0.as_ref() }
    }
}

/// Per-item playback state for an animated `source` (GIF, animated PNG, animated
/// WebP). Lives behind [`AnimatedPlaybackBox`] so it has a stable heap address for
/// the lifetime of the item, which is what makes it sound to treat `current_frame`
/// as pinned without the item itself needing to be `#[pin]`-project it.
#[derive(Default)]
pub struct AnimatedPlayback {
    current_frame: Property<u32>,
    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    timer: Timer,
    /// Which decoded source `current_frame`/`timer` are currently tracking; used to
    /// tell "the source changed" (restart from frame 0) apart from "just a `running`
    /// toggle" (resume from the frozen frame) when the change trackers below fire.
    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    current_source_key: RefCell<Option<ImageCacheKey>>,
    /// Total playback time accumulated across previous running segments, i.e.
    /// before the most recent pause or since the source last changed.
    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    elapsed_before_pause: Cell<Duration>,
    /// Instant the current running segment started; `None` while paused.
    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    running_since: Cell<Option<Instant>>,
    source_tracker: ChangeTracker,
    running_tracker: ChangeTracker,
}

impl AnimatedPlayback {
    /// The frame to display right now. Backends read this inside their tracked
    /// drawing closures, which is what makes a frame change repaint automatically.
    pub fn current_frame(&self) -> u32 {
        // Safety: see the note on `AnimatedPlaybackBox`; this struct is never moved
        // after being heap-allocated in `AnimatedPlaybackBox::default`.
        unsafe { Pin::new_unchecked(&self.current_frame) }.get()
    }

    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    fn set_current_frame(&self, frame: u32) {
        self.current_frame.set(frame);
    }

    #[cfg_attr(not(feature = "animated-images"), allow(dead_code))]
    fn elapsed(&self) -> Duration {
        match self.running_since.get() {
            Some(since) => self.elapsed_before_pause.get() + Instant::now().duration_since(since),
            None => self.elapsed_before_pause.get(),
        }
    }
}

/// Bridges `ImageItem`/`ClippedImage` for the shared animated-image playback logic
/// below. Slint's native items don't use Rust-level inheritance (each is a distinct
/// `#[repr(C)]` struct with its own duplicated fields), so this trait is what lets
/// the driver be written once instead of twice.
trait AnimatedPlaybackHost: HasStaticVTable<super::ItemVTable> {
    fn playback(self: Pin<&Self>) -> &AnimatedPlayback;
    fn running(self: Pin<&Self>) -> bool;
    fn source_image(self: Pin<&Self>) -> Image;
}

/// Sets up the two change trackers that keep `playback` synchronized with `source`
/// and `running`. Called once from `Item::init`.
fn init_playback<T: AnimatedPlaybackHost + 'static>(self_rc: &ItemRc) {
    let Some(item) = self_rc.downcast::<T>() else { return };
    let playback = item.as_pin_ref().playback();

    playback.source_tracker.init_delayed(
        self_rc.downgrade(),
        |self_weak: &ItemWeak| {
            self_weak
                .upgrade()
                .and_then(|rc| rc.downcast::<T>())
                .map_or_else(Image::default, |item| item.as_pin_ref().source_image())
        },
        |self_weak, _new_source| {
            reconcile_playback::<T>(self_weak.clone());
        },
    );

    playback.running_tracker.init_delayed(
        self_rc.downgrade(),
        |self_weak: &ItemWeak| {
            self_weak
                .upgrade()
                .and_then(|rc| rc.downcast::<T>())
                .is_some_and(|item| item.as_pin_ref().running())
        },
        |self_weak, _new_running| {
            reconcile_playback::<T>(self_weak.clone());
        },
    );
}

/// (Re)synchronizes `current_frame` and the timer with the item's current `source`
/// and `running` value. Called on startup (via the change trackers' delayed first
/// evaluation) and every time `source` or `running` changes, and re-invoked by the
/// timer itself to advance to the next frame.
///
/// The frame index is derived from wall-clock elapsed time rather than incremented
/// per tick, so a late timer callback skips frames instead of drifting the
/// animation permanently behind (see `AnimatedImage::frame_at`).
#[cfg(feature = "animated-images")]
fn reconcile_playback<T: AnimatedPlaybackHost + 'static>(self_weak: ItemWeak) {
    let Some(item_rc) = self_weak.upgrade() else { return };
    let Some(item) = item_rc.downcast::<T>() else { return };
    let item = item.as_pin_ref();
    let playback = item.playback();
    let running = item.running();
    let source = item.source_image();
    let image_inner: &ImageInner = (&source).into();

    let animated = match image_inner {
        ImageInner::AnimatedImage(a) => a.clone(),
        _ => {
            playback.timer.stop();
            *playback.current_source_key.borrow_mut() = None;
            playback.elapsed_before_pause.set(Duration::default());
            playback.running_since.set(None);
            playback.set_current_frame(0);
            return;
        }
    };

    let new_key = animated.cache_key();
    let is_new_source = *playback.current_source_key.borrow() != Some(new_key.clone());
    if is_new_source {
        *playback.current_source_key.borrow_mut() = Some(new_key);
        playback.elapsed_before_pause.set(Duration::default());
        playback.running_since.set(running.then(Instant::now));
    } else if running {
        if playback.running_since.get().is_none() {
            // Resume: keep the elapsed time accumulated before the pause, so
            // playback continues from the frozen frame rather than restarting.
            playback.running_since.set(Some(Instant::now()));
        }
    } else {
        if let Some(since) = playback.running_since.take() {
            let extra = Instant::now().duration_since(since);
            playback.elapsed_before_pause.set(playback.elapsed_before_pause.get() + extra);
        }
        playback.set_current_frame(animated.frame_at(playback.elapsed()).0 as u32);
        playback.timer.stop();
        return;
    }

    let elapsed = playback.elapsed();
    let (index, finished) = animated.frame_at(elapsed);
    playback.set_current_frame(index as u32);
    if finished {
        playback.timer.stop();
        return;
    }
    match animated.time_to_next_frame(elapsed) {
        Some(delay) => {
            playback.timer.start(TimerMode::SingleShot, delay, move || {
                reconcile_playback::<T>(self_weak.clone());
            });
        }
        None => playback.timer.stop(),
    }
}

#[cfg(not(feature = "animated-images"))]
fn reconcile_playback<T: AnimatedPlaybackHost + 'static>(_self_weak: ItemWeak) {}

#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
/// The implementation of the `Image` element
pub struct ImageItem {
    pub source: Property<crate::graphics::Image>,
    pub width: Property<LogicalLength>,
    pub height: Property<LogicalLength>,
    pub image_fit: Property<ImageFit>,
    pub image_rendering: Property<ImageRendering>,
    pub colorize: Property<Brush>,
    pub running: Property<bool>,
    pub cached_rendering_data: CachedRenderingData,
    playback: AnimatedPlaybackBox,
}

impl AnimatedPlaybackHost for ImageItem {
    fn playback(self: Pin<&Self>) -> &AnimatedPlayback {
        &*Self::FIELD_OFFSETS.playback().apply_pin(self).get_ref()
    }
    fn running(self: Pin<&Self>) -> bool {
        self.running()
    }
    fn source_image(self: Pin<&Self>) -> Image {
        self.source()
    }
}

impl Item for ImageItem {
    fn init(self: Pin<&Self>, self_rc: &ItemRc) {
        init_playback::<Self>(self_rc);
    }

    fn deinit(self: Pin<&Self>, _window_adapter: &Rc<dyn WindowAdapter>) {}

    fn layout_info(
        self: Pin<&Self>,
        orientation: Orientation,
        cross_axis_constraint: Coord,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> LayoutInfo {
        let natural_size = self.source().size();
        LayoutInfo {
            preferred: match orientation {
                _ if natural_size.width == 0 || natural_size.height == 0 => 0 as Coord,
                Orientation::Horizontal => natural_size.width as Coord,
                Orientation::Vertical => {
                    let w = if cross_axis_constraint >= 0 as Coord {
                        cross_axis_constraint
                    } else {
                        self.width().get()
                    };
                    natural_size.height as Coord * w / natural_size.width as Coord
                }
            },
            // The compiler's single-cell box layout lowering relies on image items
            // keeping the default stretch of 0 in their layout info.
            ..Default::default()
        }
    }

    fn input_event_filter_before_children(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        _: &mut super::MouseCursorInner,
    ) -> InputEventFilterResult {
        InputEventFilterResult::ForwardAndIgnore
    }

    fn input_event(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        _: &mut super::MouseCursorInner,
    ) -> InputEventResult {
        InputEventResult::EventIgnored
    }

    fn capture_key_event(
        self: Pin<&Self>,
        _: &InternalKeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        _: &InternalKeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _: &FocusEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        FocusEventResult::FocusIgnored
    }

    fn render(
        self: Pin<&Self>,
        backend: &mut &mut dyn ItemRenderer,
        self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        (*backend).draw_image(self, self_rc, size, &self.cached_rendering_data);
        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(
        self: core::pin::Pin<&Self>,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        geometry: LogicalRect,
    ) -> LogicalRect {
        geometry
    }

    fn clips_children(self: core::pin::Pin<&Self>) -> bool {
        false
    }
}

impl RenderImage for ImageItem {
    fn target_size(self: Pin<&Self>) -> LogicalSize {
        LogicalSize::from_lengths(self.width(), self.height())
    }

    fn source(self: Pin<&Self>) -> crate::graphics::Image {
        self.source()
    }

    fn source_clip(self: Pin<&Self>) -> Option<crate::graphics::IntRect> {
        None
    }

    fn image_fit(self: Pin<&Self>) -> ImageFit {
        self.image_fit()
    }

    fn rendering(self: Pin<&Self>) -> ImageRendering {
        self.image_rendering()
    }

    fn colorize(self: Pin<&Self>) -> Brush {
        self.colorize()
    }

    fn alignment(self: Pin<&Self>) -> (ImageHorizontalAlignment, ImageVerticalAlignment) {
        Default::default()
    }

    fn tiling(self: Pin<&Self>) -> (ImageTiling, ImageTiling) {
        Default::default()
    }

    fn current_frame(self: Pin<&Self>) -> u32 {
        self.playback.current_frame()
    }
}

impl ItemConsts for ImageItem {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        ImageItem,
        CachedRenderingData,
    > = ImageItem::FIELD_OFFSETS.cached_rendering_data().as_unpinned_projection();
}

#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
/// The implementation of the `ClippedImage` element
pub struct ClippedImage {
    pub source: Property<crate::graphics::Image>,
    pub width: Property<LogicalLength>,
    pub height: Property<LogicalLength>,
    pub image_fit: Property<ImageFit>,
    pub image_rendering: Property<ImageRendering>,
    pub colorize: Property<Brush>,
    pub source_clip_x: Property<i32>,
    pub source_clip_y: Property<i32>,
    pub source_clip_width: Property<i32>,
    pub source_clip_height: Property<i32>,

    pub horizontal_alignment: Property<ImageHorizontalAlignment>,
    pub vertical_alignment: Property<ImageVerticalAlignment>,
    pub horizontal_tiling: Property<ImageTiling>,
    pub vertical_tiling: Property<ImageTiling>,

    pub running: Property<bool>,

    pub cached_rendering_data: CachedRenderingData,
    playback: AnimatedPlaybackBox,
}

impl AnimatedPlaybackHost for ClippedImage {
    fn playback(self: Pin<&Self>) -> &AnimatedPlayback {
        &*Self::FIELD_OFFSETS.playback().apply_pin(self).get_ref()
    }
    fn running(self: Pin<&Self>) -> bool {
        self.running()
    }
    fn source_image(self: Pin<&Self>) -> Image {
        self.source()
    }
}

impl Item for ClippedImage {
    fn init(self: Pin<&Self>, self_rc: &ItemRc) {
        init_playback::<Self>(self_rc);
    }

    fn deinit(self: Pin<&Self>, _window_adapter: &Rc<dyn WindowAdapter>) {}

    fn layout_info(
        self: Pin<&Self>,
        orientation: Orientation,
        cross_axis_constraint: Coord,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> LayoutInfo {
        LayoutInfo {
            preferred: match orientation {
                Orientation::Horizontal => self.source_clip_width() as Coord,
                Orientation::Vertical => {
                    let source_clip_width = self.source_clip_width();
                    if source_clip_width == 0 {
                        0 as Coord
                    } else {
                        let w = if cross_axis_constraint >= 0 as Coord {
                            cross_axis_constraint
                        } else {
                            self.width().get()
                        };
                        self.source_clip_height() as Coord * w / source_clip_width as Coord
                    }
                }
            },
            ..Default::default()
        }
    }

    fn input_event_filter_before_children(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        _: &mut super::MouseCursorInner,
    ) -> InputEventFilterResult {
        InputEventFilterResult::ForwardAndIgnore
    }

    fn input_event(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        _: &mut super::MouseCursorInner,
    ) -> InputEventResult {
        InputEventResult::EventIgnored
    }

    fn capture_key_event(
        self: Pin<&Self>,
        _: &InternalKeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        _: &InternalKeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _: &FocusEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        FocusEventResult::FocusIgnored
    }

    fn render(
        self: Pin<&Self>,
        backend: &mut &mut dyn ItemRenderer,
        self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        (*backend).draw_image(self, self_rc, size, &self.cached_rendering_data);
        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(
        self: core::pin::Pin<&Self>,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        geometry: LogicalRect,
    ) -> LogicalRect {
        geometry
    }

    fn clips_children(self: core::pin::Pin<&Self>) -> bool {
        false
    }
}

impl RenderImage for ClippedImage {
    fn target_size(self: Pin<&Self>) -> LogicalSize {
        LogicalSize::from_lengths(self.width(), self.height())
    }

    fn source(self: Pin<&Self>) -> crate::graphics::Image {
        self.source()
    }

    fn source_clip(self: Pin<&Self>) -> Option<crate::graphics::IntRect> {
        Some(euclid::rect(
            self.source_clip_x(),
            self.source_clip_y(),
            self.source_clip_width(),
            self.source_clip_height(),
        ))
    }

    fn image_fit(self: Pin<&Self>) -> ImageFit {
        self.image_fit()
    }

    fn rendering(self: Pin<&Self>) -> ImageRendering {
        self.image_rendering()
    }

    fn colorize(self: Pin<&Self>) -> Brush {
        self.colorize()
    }

    fn alignment(self: Pin<&Self>) -> (ImageHorizontalAlignment, ImageVerticalAlignment) {
        (self.horizontal_alignment(), self.vertical_alignment())
    }

    fn tiling(self: Pin<&Self>) -> (ImageTiling, ImageTiling) {
        (self.horizontal_tiling(), self.vertical_tiling())
    }

    fn current_frame(self: Pin<&Self>) -> u32 {
        self.playback.current_frame()
    }
}

impl ItemConsts for ClippedImage {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        ClippedImage,
        CachedRenderingData,
    > = ClippedImage::FIELD_OFFSETS.cached_rendering_data().as_unpinned_projection();
}
