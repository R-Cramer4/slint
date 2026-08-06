// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Decoding and playback-timing support for animated raster images (GIF, animated
//! PNG, animated WebP).

use super::{ImageCacheKey, ImageInner, OpaqueImage, SharedImageBuffer};
use crate::graphics::IntSize;
use alloc::vec::Vec;
use core::num::NonZeroU32;
use core::time::Duration;

/// How many times an animated image should repeat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoopCount {
    /// Loop forever.
    Infinite,
    /// Loop the given number of times, then hold the last frame.
    Finite(NonZeroU32),
}

/// A decoded multi-frame raster image.
///
/// All frames share the size of the first frame: the `image` crate's animation
/// decoders already composite disposal and alpha blending into full-canvas RGBA
/// frames, so Slint never implements per-format disposal logic itself.
///
/// The frame index for a point in time is derived from wall-clock elapsed time
/// rather than incremented per timer tick, so a late timer tick skips frames
/// instead of drifting the animation permanently behind.
pub struct AnimatedImage {
    frames: Vec<SharedImageBuffer>,
    /// Cumulative end time of each frame within one loop, in milliseconds.
    /// `frame_ends_ms[i]` is when frame `i` stops being current.
    frame_ends_ms: Vec<u32>,
    loop_count: LoopCount,
    size: IntSize,
    cache_key: ImageCacheKey,
    weight_in_bytes: usize,
}

impl core::fmt::Debug for AnimatedImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AnimatedImage").field("frame_count", &self.frames.len()).finish()
    }
}

impl OpaqueImage for AnimatedImage {
    fn size(&self) -> IntSize {
        self.size()
    }
    fn cache_key(&self) -> ImageCacheKey {
        self.cache_key()
    }
}

impl AnimatedImage {
    fn new(
        frames: Vec<(SharedImageBuffer, u32)>,
        loop_count: LoopCount,
        cache_key: ImageCacheKey,
    ) -> Option<Self> {
        if frames.len() < 2 {
            return None;
        }
        let size = frames[0].0.size();
        let weight_in_bytes = frames.iter().map(|(buffer, _)| buffer_len_bytes(buffer)).sum();
        let mut acc: u32 = 0;
        let mut frame_ends_ms = Vec::with_capacity(frames.len());
        let frames = frames
            .into_iter()
            .map(|(buffer, delay_ms)| {
                acc = acc.saturating_add(delay_ms);
                frame_ends_ms.push(acc);
                buffer
            })
            .collect();
        Some(Self { frames, frame_ends_ms, loop_count, size, cache_key, weight_in_bytes })
    }

    /// Number of frames in the animation.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Returns the size shared by every frame.
    pub fn size(&self) -> IntSize {
        self.size
    }

    /// The cache key this animated image was decoded under.
    pub fn cache_key(&self) -> ImageCacheKey {
        self.cache_key.clone()
    }

    /// Approximate number of bytes the decoded frames keep alive, for cache accounting.
    pub fn weight_in_bytes(&self) -> usize {
        self.weight_in_bytes
    }

    /// Duration of a single loop through all frames.
    pub fn total_duration(&self) -> Duration {
        Duration::from_millis(*self.frame_ends_ms.last().unwrap_or(&0) as u64)
    }

    /// Returns the pixels for the given frame index, clamped to the last frame.
    pub fn frame(&self, index: usize) -> SharedImageBuffer {
        self.frames[index.min(self.frames.len() - 1)].clone()
    }

    /// Maps elapsed time since the animation started to a frame index, honouring the
    /// loop count. Returns `(index, finished)`; once a finite loop count has been
    /// exhausted, `finished` is `true` and `index` is the last frame.
    pub fn frame_at(&self, elapsed: Duration) -> (usize, bool) {
        let total_ms = self.total_duration().as_millis() as u64;
        if total_ms == 0 {
            return (self.frames.len() - 1, true);
        }
        let elapsed_ms = elapsed.as_millis() as u64;
        let finished = match self.loop_count {
            LoopCount::Infinite => false,
            LoopCount::Finite(n) => elapsed_ms >= total_ms.saturating_mul(n.get() as u64),
        };
        if finished {
            return (self.frames.len() - 1, true);
        }
        let loop_elapsed_ms = (elapsed_ms % total_ms) as u32;
        let index = self.frame_ends_ms.partition_point(|&end| end <= loop_elapsed_ms);
        (index.min(self.frames.len() - 1), false)
    }

    /// Duration from `elapsed` until the frame index changes, or `None` once the
    /// animation has finished (a finite loop count has been exhausted).
    pub fn time_to_next_frame(&self, elapsed: Duration) -> Option<Duration> {
        let (index, finished) = self.frame_at(elapsed);
        if finished {
            return None;
        }
        let total_ms = self.total_duration().as_millis() as u64;
        let loop_elapsed_ms = (elapsed.as_millis() as u64 % total_ms) as u32;
        let next_ms = self.frame_ends_ms[index].saturating_sub(loop_elapsed_ms).max(1);
        Some(Duration::from_millis(next_ms as u64))
    }
}

fn buffer_len_bytes(buffer: &SharedImageBuffer) -> usize {
    match buffer {
        SharedImageBuffer::RGB8(pixels) => pixels.as_bytes().len(),
        SharedImageBuffer::RGBA8(pixels) => pixels.as_bytes().len(),
        SharedImageBuffer::RGBA8Premultiplied(pixels) => pixels.as_bytes().len(),
    }
}

/// GIFs in the wild routinely specify delays under 20ms, which legacy tooling treated
/// as "as fast as possible" but which every modern viewer clamps; the `image` crate
/// reports the raw value. APNG/WebP tooling has no such legacy quirk, so their delays
/// are used as decoded.
const GIF_MIN_DELAY_MS: u32 = 20;
const GIF_CLAMPED_DELAY_MS: u32 = 100;

/// Attempts to decode `reader` as a multi-frame GIF, animated PNG or animated WebP.
///
/// Returns `None` if `format` isn't one of those three, or if the file turns out not
/// to be animated (e.g. a plain PNG, or a GIF/WebP with a single frame) — the caller
/// then falls back to the ordinary single-frame decode path. A single-frame result
/// becomes a plain [`ImageInner::EmbeddedImage`] rather than an `AnimatedImage`, so
/// static images keep using the well-trodden single-frame path (including, for
/// backends that have one, its cache-sharing optimizations).
#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
pub(crate) fn try_load_animated<R: std::io::BufRead + std::io::Seek>(
    reader: R,
    format: Option<image::ImageFormat>,
    cache_key: ImageCacheKey,
) -> Option<ImageInner> {
    match format? {
        image::ImageFormat::Gif => decode_gif(reader, cache_key),
        image::ImageFormat::Png => decode_apng(reader, cache_key),
        image::ImageFormat::WebP => decode_webp(reader, cache_key),
        _ => None,
    }
}

#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
fn decode_gif<R: std::io::BufRead + std::io::Seek>(
    reader: R,
    cache_key: ImageCacheKey,
) -> Option<ImageInner> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;
    let decoder = GifDecoder::new(reader).ok()?;
    let loop_count = convert_loop_count(decoder.loop_count());
    let frames = match decoder.into_frames().collect_frames() {
        Ok(frames) => frames,
        Err(err) => {
            crate::debug_log!("Error decoding animated GIF: {}", err);
            return None;
        }
    };
    build_image_inner(frames, loop_count, cache_key, true)
}

#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
fn decode_apng<R: std::io::BufRead + std::io::Seek>(
    reader: R,
    cache_key: ImageCacheKey,
) -> Option<ImageInner> {
    use image::AnimationDecoder;
    use image::codecs::png::PngDecoder;
    let decoder = PngDecoder::new(reader).ok()?;
    if !decoder.is_apng().unwrap_or(false) {
        return None;
    }
    let decoder = decoder.apng().ok()?;
    let loop_count = convert_loop_count(decoder.loop_count());
    let frames = match decoder.into_frames().collect_frames() {
        Ok(frames) => frames,
        Err(err) => {
            crate::debug_log!("Error decoding animated PNG: {}", err);
            return None;
        }
    };
    build_image_inner(frames, loop_count, cache_key, false)
}

#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
fn decode_webp<R: std::io::BufRead + std::io::Seek>(
    reader: R,
    cache_key: ImageCacheKey,
) -> Option<ImageInner> {
    use image::AnimationDecoder;
    use image::codecs::webp::WebPDecoder;
    let decoder = WebPDecoder::new(reader).ok()?;
    if !decoder.has_animation() {
        return None;
    }
    let loop_count = convert_loop_count(decoder.loop_count());
    let frames = match decoder.into_frames().collect_frames() {
        Ok(frames) => frames,
        Err(err) => {
            crate::debug_log!("Error decoding animated WebP: {}", err);
            return None;
        }
    };
    build_image_inner(frames, loop_count, cache_key, false)
}

#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
fn convert_loop_count(loop_count: image::metadata::LoopCount) -> LoopCount {
    match loop_count {
        image::metadata::LoopCount::Infinite => LoopCount::Infinite,
        image::metadata::LoopCount::Finite(n) => LoopCount::Finite(n),
    }
}

#[cfg(all(feature = "animated-images", not(target_arch = "wasm32")))]
fn build_image_inner(
    frames: Vec<image::Frame>,
    loop_count: LoopCount,
    cache_key: ImageCacheKey,
    clamp_gif_delays: bool,
) -> Option<ImageInner> {
    let mut frames: Vec<(SharedImageBuffer, u32)> = frames
        .into_iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let mut delay_ms = numer / denom.max(1);
            if clamp_gif_delays && delay_ms < GIF_MIN_DELAY_MS {
                delay_ms = GIF_CLAMPED_DELAY_MS;
            }
            let buffer = super::dynamic_image_to_shared_image_buffer(
                image::DynamicImage::ImageRgba8(frame.into_buffer()),
            );
            (buffer, delay_ms)
        })
        .collect();

    if frames.len() < 2 {
        // Q9: a single-frame animated-format file is just a static image; keep it on
        // the ordinary EmbeddedImage path rather than growing an AnimatedImage of one.
        let (buffer, _) = frames.pop()?;
        return Some(ImageInner::EmbeddedImage { cache_key, buffer });
    }

    AnimatedImage::new(frames, loop_count, cache_key)
        .map(|animated| ImageInner::AnimatedImage(vtable::VRc::new(animated)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn frame_buffer() -> SharedImageBuffer {
        SharedImageBuffer::RGB8(crate::graphics::SharedPixelBuffer::new(1, 1))
    }

    fn animated(delays_ms: &[u32], loop_count: LoopCount) -> AnimatedImage {
        let frames = delays_ms.iter().map(|&d| (frame_buffer(), d)).collect();
        AnimatedImage::new(frames, loop_count, ImageCacheKey::Invalid).unwrap()
    }

    #[test]
    fn single_frame_is_rejected() {
        assert!(
            AnimatedImage::new(
                vec![(frame_buffer(), 100)],
                LoopCount::Infinite,
                ImageCacheKey::Invalid
            )
            .is_none()
        );
    }

    #[test]
    fn irregular_delays_infinite_loop() {
        let anim = animated(&[100, 50, 200], LoopCount::Infinite);
        assert_eq!(anim.total_duration(), Duration::from_millis(350));
        assert_eq!(anim.frame_at(Duration::from_millis(0)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(99)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(100)), (1, false));
        assert_eq!(anim.frame_at(Duration::from_millis(149)), (1, false));
        assert_eq!(anim.frame_at(Duration::from_millis(150)), (2, false));
        assert_eq!(anim.frame_at(Duration::from_millis(349)), (2, false));
        // Wraps back to frame 0 at the loop boundary.
        assert_eq!(anim.frame_at(Duration::from_millis(350)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(450)), (1, false));
        assert_eq!(anim.frame_at(Duration::from_millis(1050)), (0, false));
    }

    #[test]
    fn time_to_next_frame_infinite() {
        let anim = animated(&[100, 50, 200], LoopCount::Infinite);
        assert_eq!(
            anim.time_to_next_frame(Duration::from_millis(0)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            anim.time_to_next_frame(Duration::from_millis(50)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            anim.time_to_next_frame(Duration::from_millis(149)),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            anim.time_to_next_frame(Duration::from_millis(349)),
            Some(Duration::from_millis(1))
        );
    }

    #[test]
    fn finite_loop_holds_last_frame() {
        let anim = animated(&[100, 100], LoopCount::Finite(NonZeroU32::new(2).unwrap()));
        assert_eq!(anim.total_duration(), Duration::from_millis(200));
        assert_eq!(anim.frame_at(Duration::from_millis(0)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(150)), (1, false));
        // 250ms in is 50ms into the second loop iteration: back to frame 0.
        assert_eq!(anim.frame_at(Duration::from_millis(250)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(399)), (1, false));
        // Exactly exhausted: finished, holds the last frame.
        assert_eq!(anim.frame_at(Duration::from_millis(400)), (1, true));
        assert_eq!(anim.frame_at(Duration::from_millis(10_000)), (1, true));
        assert_eq!(anim.time_to_next_frame(Duration::from_millis(400)), None);
    }

    #[test]
    fn finite_single_loop() {
        let anim = animated(&[100, 100], LoopCount::Finite(NonZeroU32::new(1).unwrap()));
        assert_eq!(anim.frame_at(Duration::from_millis(50)), (0, false));
        assert_eq!(anim.frame_at(Duration::from_millis(199)), (1, false));
        assert_eq!(anim.frame_at(Duration::from_millis(200)), (1, true));
    }

    #[test]
    fn zero_delay_frames_are_skipped_over() {
        let anim = animated(&[0, 100], LoopCount::Infinite);
        // The zero-duration first frame is instantaneous; any elapsed time lands on frame 1.
        assert_eq!(anim.frame_at(Duration::from_millis(0)), (1, false));
    }
}
