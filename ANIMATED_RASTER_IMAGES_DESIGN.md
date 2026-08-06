# Design: animated raster images — GIF / APNG / animated WebP (#2081)

Status: **design proposal, not yet approved.** Every unsettled choice is written
up as a numbered open question in §11 with the trade-offs and a recommendation.
Read §11 first if you only want the decisions.

Scope: GIF, animated PNG (APNG) and animated WebP. Lottie (#5549) is
deliberately out of scope — it needs a different `ImageInner` shape (parse-once,
render-for-(time, size), like SVG-plus-a-time-axis) and a much richer control
API. It is mentioned only where a decision here would box Lottie in later.

All file/line references were checked against the working tree at the time of
writing (branch `animated-images`).

---

## 1. Summary

The pieces Slint already has:

- a decode-time `ImageInner` enum with a precedent (`Svg`) for "a decoded thing
  that is not just a pixel buffer";
- a universal type-erased pixel funnel, `ImageInner::render_to_buffer`, which
  **four of the five backends already route through** with a catch-all match
  arm;
- a property system that turns "a property read during `draw_image` changed"
  into "repaint exactly this item's dirty region", automatically, in every
  backend including the software partial renderer;
- `Timer`, plus an exact precedent for "timer flips a property, UI repaints"
  in `TextCursorBlinker` (`internal/core/input.rs:1791-1863`);
- an exact precedent for "builtin item carries opaque non-property state and
  reacts to property changes" in `SystemTrayIcon`
  (`internal/core/items/system_tray.rs:141-147`, `:260`).

The pieces that are missing, and that this document designs:

1. multi-frame decode (both decode entry points currently keep only frame 0);
2. a decoded representation that holds frames + per-frame delays + loop count;
3. per-item playback state and a driver that advances it;
4. a way for the chosen frame index to reach the pixels, given that
   `render_to_buffer` has no frame parameter;
5. cache-key and cache-accounting updates in the backends that keep derived
   resources (femtovg textures, anyrender `peniko::ImageData`).

Point 4 is the single most consequential decision in the whole design; it is
open question **Q2**.

---

## 2. Corrections to the prior feasibility report

The earlier `ANIMATED_IMAGES_FEASIBILITY.md` was broadly right. These points
were verified and differ:

| Claim in the feasibility report | Verified reality |
|---|---|
| "Decode dispatch is `load_from_data_with_cache_key` (`image.rs:566`)" | There are **two** entry points. `load_from_data_with_cache_key` (`image.rs:566`) handles embedded/data-URI bytes; `ImageCache::load_image_from_path` (`cache.rs:83`) handles files and calls `image::open()` directly. Both need multi-frame dispatch, and they need *different* code (`&[u8]` vs. path/reader). |
| "Each of the 5 backends needs a new match arm, almost all mechanical" | Only **Skia** and **anyrender** genuinely need a new match arm (both currently have a silent `_ => None` fallback: `skia/cached_image.rs:106-107`, `anyrender/itemrenderer.rs:1290-1291`). **femtovg** (`femtovg/images.rs:211-215`), **software** (`software/lib.rs:2400-2461`) and **Qt** (`qt_window.rs:1639-1644`) all funnel unknown variants into `render_to_buffer` and need no new arm — only frame-aware cache keys. |
| "The software renderer re-rasterizes SVG every frame with no caching — a pre-existing gap" | True, and for animated images it is a *feature*, not a gap: it means the software renderer needs literally zero changes, because it asks for pixels every frame anyway. |
| "wasm: unverified whether the `<img>` is drawn live or snapshotted" | **Snapshotted.** `femtovg/images.rs:142-161` uploads `html_image.dom_element` into a GL texture once and caches it in `TextureCache` keyed by `ImageCacheKey`. A browser-animated GIF would freeze at whatever frame was current at upload time. See §9. |
| "`image-default-formats` is the umbrella flag end users toggle" | Correct, but note it is **not** in `slint`'s default features (`api/rs/slint/Cargo.toml:22-31`). Out of the box a Rust app can only decode PNG and JPEG (workspace `Cargo.toml:119` pins `image` with `default-features = false, features = ["png", "jpeg"]`). GIF and WebP are not decodable at all by default today. This changes the feature-flag discussion (Q8). |
| "GIF: every GIF can go through the animated path uniformly" | True and worth keeping, but the same is *not* true for PNG/WebP — see §4. |
| "`loop_count` metadata is a v1.1" | The `image` crate hands it to us for free on all three formats (`AnimationDecoder::loop_count()`), so the cost of *capturing* it at decode time is zero. Only the *semantics* need deciding (Q7). |
| "`ImageInner::AnimatedImage(...) = 9`" | Discriminant 9 is free (`WGPUTexture` is 8), but note the enum is cbindgen-exported to C++ as a tagged union (`api/cpp/cbindgen.rs:504`, `api/cpp/include/private/slint_image.h:297`), so this is an ABI-visible addition. |

Two hazards the report did not mention:

- **Skia poisons the decode cache.** `as_skia_image`'s `EmbeddedImage` arm calls
  `core_cache::replace_cached_image()` (`skia/cached_image.rs:42-53`) to swap the
  decoded `ImageInner` in the *global* decode cache for an
  `ImageInner::BackendStorage` holding an `SkImage`. If an animated variant ever
  took that path, the animation would be destroyed on first draw. The new arm
  must not do this.
- **The 5 MiB decode cache silently drops oversized entries.**
  `lookup_image_in_cache_or_create` (`cache.rs:63-75`) does
  `put_with_weight(...).ok()` and returns the image regardless. An animated
  image weighing more than the cap is therefore *not cached* — correct behaviour,
  but it means every `Image` element with that source decodes its own copy.

---

## 3. Current architecture, as it actually is

### 3.1 `ImageInner` and the two decode entry points

```rust
// internal/core/graphics/image.rs:420-439
pub enum ImageInner {
    None = 0,
    EmbeddedImage { cache_key: ImageCacheKey, buffer: SharedImageBuffer } = 1,
    #[cfg(feature = "svg")]
    Svg(vtable::VRc<OpaqueImageVTable, svg::ParsedSVG>) = 2,
    StaticTextures(&'static StaticTextures) = 3,
    #[cfg(target_arch = "wasm32")]
    HTMLImage(vtable::VRc<OpaqueImageVTable, htmlimage::HTMLImage>) = 4,
    BackendStorage(vtable::VRc<OpaqueImageVTable>) = 5,
    #[cfg(not(target_arch = "wasm32"))]
    BorrowedOpenGLTexture(BorrowedOpenGLTexture) = 6,
    NineSlice(vtable::VRc<OpaqueImageVTable, NineSliceImage>) = 7,
    #[cfg(any(feature = "unstable-wgpu-29", feature = "unstable-wgpu-30"))]
    WGPUTexture(WGPUTexture) = 8,
}
```

Decode entry point 1 — **from bytes** (`image.rs:566-632`): SVG sniffing first,
then `image::load_from_memory[_with_format]` → one `DynamicImage` →
`dynamic_image_to_shared_image_buffer` → `EmbeddedImage`.

Decode entry point 2 — **from a path** (`cache.rs:83-115`): `.svg`/`.svgz`
extension check, then `image::open(path)` → one `DynamicImage` →
`EmbeddedImage`.

Both discard everything but the first frame. That is the whole of why #2081
does not work.

### 3.2 `render_to_buffer`: the universal pixel funnel

```rust
// internal/core/graphics/image.rs:448
pub fn render_to_buffer(
    &self,
    _target_size_for_scalable_source: Option<euclid::Size2D<u32, PhysicalPx>>,
) -> Option<SharedImageBuffer>
```

Who calls it, and with what fallback behaviour for an unknown variant:

| Backend | Call site | Unknown variant today |
|---|---|---|
| software | `software/lib.rs:2415` (inside the `_ =>` arm at `:2400`) | **handled** — renders via `render_to_buffer` every frame, no cache |
| Qt | `qt_window.rs:1643` (`image_to_pixmap`) | **handled** — `render_to_buffer` then `QPixmap` |
| femtovg | `femtovg/images.rs:212` (inside the `_ =>` arm at `:211`) | **handled** — `render_to_buffer` then GL upload |
| Skia | `skia/cached_image.rs:31` (`as_skia_image`) | **`_ => None` at `:106`** — draws nothing, silently |
| anyrender | `anyrender/itemrenderer.rs:1214` (`load_image`) | **`_ => None` at `:1290`** — draws nothing, silently |

This is the reason "add one variant, get three backends free" is realistic — and
also the reason Q2 (how the frame index reaches this function) matters so much.

### 3.3 Three distinct caching layers

**Layer 1 — decode cache** (`internal/core/graphics/image/cache.rs`). Thread-local
`clru::CLruCache<ImageCacheKey, ImageInner, _, ImageWeightInBytes>`, capacity
**5 MiB** (`cache.rs:49-58`). Keyed by path+mtime / embedded-data pointer / URL.
Holds `ImageInner` *strongly*. Weights come from `ImageWeightInBytes::weight`
(`cache.rs:15-37`), which needs a new arm.

**Layer 2 — per-item render cache** (`ItemCache<T>`,
`internal/core/item_rendering.rs:36-89`). A
`HashMap<component_ptr, HashMap<item_index, CachedGraphicsData<T>>>` where each
entry owns a `PropertyTracker`. `get_or_update_cache_entry` re-runs the update
closure only if a property read during the previous run went dirty. Used by
femtovg (`femtovg/itemrenderer.rs:1311`), Skia (`skia/itemrenderer.rs:352`),
anyrender (`anyrender/itemrenderer.rs:248`) and Qt (`qt_window.rs:1689`).
**Not** used by the software renderer.

**Layer 3 — per-backend shared derived-resource caches**, deduplicating across
items:

- femtovg `TextureCache<TextureCacheKey>` (`femtovg/images.rs:228-256`), key =
  `{ source_key: ImageCacheKey, target_size_for_scalable_source, gpu_image_flags,
  gpu_image_tiling }`;
- anyrender `ImageConversionCache` (`anyrender/imagecache.rs:49`), key =
  `(ImageCacheKey, ImageVariant)` where
  `ImageVariant ∈ { Full, Sized{w,h}, Tile{..} }` (`imagecache.rs:35-45`);
- Skia and Qt have no layer-3 cache for images (Skia's `replace_cached_image`
  trick abuses layer 1 as one).

Layer 3 is where the frame index *must* appear, or two items showing different
frames of the same GIF will get each other's pixels.

### 3.4 How a changed pixel becomes a repaint (the mechanism to exploit)

`PartialRenderer::do_rendering` (`internal/core/partial_renderer.rs:590-609`)
wraps **every** `draw_*` call in a per-item `PropertyTracker::evaluate`:

```rust
entry.tracker
    .get_or_insert_with(|| Box::pin(PropertyTracker::default()))
    .as_ref()
    .evaluate(render_fn);
```

So *any* `Property` read anywhere inside `draw_image` — including deep inside
`render_to_buffer` — is registered as a dependency of that item's rendering.
When it changes, the item's bounding rect enters the dirty region and the window
repaints just that area. This works identically for all backends and is the
piece that makes the whole feature cheap.

`ItemCache::get_or_update_cache_entry` (layer 2) does the same thing for the
backends that have it. So a frame-index property read inside the update closure
invalidates the cached texture/`SkImage`/`QPixmap` automatically too.

The event-loop side is already wired: `about_to_wait`
(`internal/backends/winit/event_loop.rs:618-652`) both requests a redraw for
windows with active animations *and* shortens the wait to
`duration_until_next_timer_update()`, so a pending `Timer` wakes the loop.

### 3.5 Two precedents worth copying verbatim

**`TextCursorBlinker`** (`internal/core/input.rs:1791-1863`) — the canonical
"timer drives a property drives a repaint" shape:

```rust
pub(crate) struct TextCursorBlinker {
    cursor_visible: Property<bool>,
    cursor_blink_timer: crate::timers::Timer,
}
```

The timer callback holds a `PinWeak` back to the struct and flips the property;
consumers `set_binding` onto it. Nothing else is needed to get repaints.

**`SystemTrayIcon`** (`internal/core/items/system_tray.rs:141-147, 189-196,
260-…`) — the canonical "builtin item with opaque state that reacts to a
`Property<Image>` changing":

```rust
pub struct SystemTrayIconData {
    inner: OnceCell<SystemTrayIconHandle>,
    change_tracker: ChangeTracker,
    icon_tracker: ChangeTracker,
    ...
}
```

exposed to C++ as an opaque forward declaration via
`config.export.pre_body.insert("SystemTrayIconDataBox", "struct SystemTrayIconData;")`
(`api/cpp/cbindgen.rs:963-966`). `Flickable`/`FlickableDataBox` and
`Path`/`FittedPathBox` use the same trick. This is exactly the escape hatch an
`ImageItem` needs to carry a `Timer` and a frame index.

---

## 4. What the `image` crate gives us (verified against 0.25.10)

`Cargo.lock` pins `image 0.25.10`, which pulls `gif 0.14.2`, `png 0.18.1` and
`image-webp 0.2.4`.

```rust
// image 0.25.10, src/io/decoder.rs:199
pub trait AnimationDecoder<'a> {
    fn into_frames(self) -> Frames<'a>;
    fn loop_count(&self) -> LoopCount { LoopCount::Finite(NonZeroU32::new(1).unwrap()) }
}
```

Implemented for `GifDecoder` (`codecs/gif.rs:426`), `ApngDecoder`
(`codecs/png.rs:514`) and `WebPDecoder` (`codecs/webp/decoder.rs:104`).

`Frame` (`src/animation.rs:37`) carries `delay: Delay`, `left`, `top` and a
`buffer: RgbaImage`. Crucial detail: **the decoders already composite.** GIF's
iterator applies disposal method and alpha blending and emits full-canvas frames
at `(left, top) == (0, 0)` (`codecs/gif.rs:352-419`); WebP likewise reads into a
full-size `RgbaImage`. So Slint never has to implement GIF disposal logic — every
frame arrives as a complete, ready-to-blit RGBA canvas of the image's full size.
That also means all frames share one size, so `Image::size()` and layout are
unaffected by which frame is showing.

`LoopCount` (`src/metadata.rs:166-171`) is `Infinite | Finite(NonZeroU32)`, and
all three decoders read it from the file (GIF `NETSCAPE` repeat, APNG `acTL`
`num_plays`, WebP `ANIM` loop count).

**Per-format animation detection**, which is where the three differ:

| Format | Cheap "is it animated?" check | Notes |
|---|---|---|
| GIF | none needed | Every GIF can go through `GifDecoder::into_frames()`. A single-frame GIF yields one frame. Uniform path. |
| PNG | `PngDecoder::is_apng() -> ImageResult<bool>` (`codecs/png.rs:160`) | Most PNGs are not APNG. Must check before `.apng()`, otherwise the frame iterator is empty (`codecs/png.rs:159` documents exactly this). |
| WebP | `WebPDecoder::has_animation() -> bool` (`codecs/webp/decoder.rs:31`) | Same shape as PNG. |

Both checks are cheap header reads, so the dispatch is: construct the
format-specific decoder, ask, and branch. Note this means the decode path can no
longer be a single `image::load_from_memory` call; it becomes a small
format-dispatch function (see §5.2).

Cargo features on the `image` crate: `gif`, `png`, `webp`. Today the workspace
enables only `png` and `jpeg` by default; `gif` and `webp` arrive only via
`image-default-formats` → `image/default-formats`.

---

## 5. Design

### 5.1 The decoded representation

New module `internal/core/graphics/image/animated.rs`, mirroring
`svg.rs`'s shape (an `OpaqueImage` impl, a `weight_in_bytes`, a `cache_key`):

```rust
pub struct AnimatedImage {
    /// One fully-composited RGBA8-premultiplied canvas per frame.
    /// All frames share the same dimensions (the decoders guarantee this).
    frames: Vec<SharedImageBuffer>,
    /// Cumulative end-time of each frame, in milliseconds from animation start.
    /// `frame_ends[i]` = when frame `i` stops being current.
    /// Stored cumulative (not per-frame) so frame lookup is a binary search
    /// and does not accumulate rounding error over long animations.
    frame_ends: Vec<u32>,
    loop_count: LoopCount,          // Infinite | Finite(NonZeroU32)
    size: IntSize,
    cache_key: ImageCacheKey,
    weight_in_bytes: usize,
}

impl AnimatedImage {
    pub fn frame_count(&self) -> usize;
    pub fn total_duration(&self) -> Duration;      // one loop
    pub fn frame(&self, index: usize) -> SharedImageBuffer;   // clamped, cheap clone
    /// Map elapsed-since-start to a frame index, honouring `loop_count`.
    /// Returns `(index, finished)`; `finished` is true once a finite loop
    /// count has been exhausted, in which case `index` is the last frame.
    pub fn frame_at(&self, elapsed: Duration) -> (usize, bool);
    /// Time from `elapsed` until the frame index changes, or `None` if finished.
    pub fn time_to_next_frame(&self, elapsed: Duration) -> Option<Duration>;
}
```

and

```rust
#[cfg(feature = "animated-images")]
AnimatedImage(vtable::VRc<OpaqueImageVTable, animated::AnimatedImage>) = 9,
```

Storing cumulative end-times rather than per-frame delays, and deriving the
frame index from *wall-clock elapsed time* rather than incrementing a counter on
each timer tick, is deliberate: a late timer tick then skips frames instead of
letting the animation drift permanently behind. It also makes "seek to time T"
free, which is what deterministic testing (§10) and any future Lottie work need.

Zero-delay frames (common in GIFs produced by exporters) need clamping — browsers
clamp delays below 10 ms to 100 ms for compatibility. See Q10.

**Charging the decode cache** (`cache.rs:15-37`):

```rust
#[cfg(feature = "animated-images")]
ImageInner::AnimatedImage(a) => a.weight_in_bytes(),  // = Σ frame byte lengths
```

### 5.2 Decode dispatch

Both entry points get a shared helper. Sketch for the bytes path
(`image.rs:566-632`), replacing the `image::load_from_memory` block:

```rust
let format = std::str::from_utf8(format.as_slice()).ok()
    .and_then(image::ImageFormat::from_extension)
    .or_else(|| image::guess_format(data.as_slice()).ok());

#[cfg(feature = "animated-images")]
if let Some(inner) = animated::try_load_animated(data.as_slice(), format, &cache_key) {
    return Some(inner);   // ImageInner::AnimatedImage, or None if not animated
}

// ... existing single-frame path unchanged
```

`try_load_animated` is the only place that knows about per-format detection:

```rust
match format {
    Some(ImageFormat::Gif)  => decode_all(GifDecoder::new(cursor)?),
    Some(ImageFormat::Png)  => { let d = PngDecoder::new(cursor)?;
                                 d.is_apng()?.then(|| decode_all(d.apng()?)) }
    Some(ImageFormat::WebP) => { let d = WebPDecoder::new(cursor)?;
                                 d.has_animation().then(|| decode_all(d)) }
    _ => None,
}
```

Note the format must be *known* here. Today the byte path falls back to
`image::load_from_memory`'s internal sniffing when the extension is absent;
`try_load_animated` needs `image::guess_format` explicitly. The path entry point
(`cache.rs:83-115`) does the same over a `BufReader<File>` instead of a
`Cursor<&[u8]>`, which is strictly better — the animated decoders want
`BufRead + Seek` and a file gives that without buffering the whole thing.

**Open**: whether a *single-frame* GIF/APNG/WebP should become an
`AnimatedImage` with one frame or fall through to `EmbeddedImage`. See Q9.

**Interaction with the `svg` early-return**: unchanged; SVG sniffing still runs
first in both entry points.

### 5.3 Playback state: where it lives

This is the design's load-bearing choice. Three coherent options; the difference
between them is entirely about *who owns the current-frame value*.

#### Option A — shared, on the decoded image (cheapest)

Put `current_frame: Property<u32>` and a `Timer` inside `AnimatedImage`.
`render_to_buffer` reads the property and returns `frames[current_frame]`.

- **Backend cost: nearly zero.** The property read happens inside every
  backend's tracked closure (§3.4), so invalidation and repaint are automatic.
  Only layer-3 cache keys need the frame index.
- **`RenderImage`, `ImageItem` and the C++ ABI are untouched.**
- **But**: all `Image` elements pointing at the same file necessarily show the
  same frame, in the same phase. A per-element `running` is impossible.
- **And**: the timer is owned by an object the *decode cache* holds strongly for
  up to 5 MiB, so the animation keeps ticking (and keeps marking items dirty)
  after every `Image` element using it is gone, until the cache evicts it.

#### Option B — per-item, on `ImageItem` (recommended)

Add an opaque data box to `ImageItem`, exactly as `Flickable`/`SystemTrayIcon`
do:

```rust
pub struct ImageItem {
    pub source: Property<Image>,
    pub width: Property<LogicalLength>,
    pub height: Property<LogicalLength>,
    pub image_fit: Property<ImageFit>,
    pub image_rendering: Property<ImageRendering>,
    pub colorize: Property<Brush>,
    pub running: Property<bool>,          // new, defaults true via builtins.slint
    pub cached_rendering_data: CachedRenderingData,
    playback: AnimatedPlaybackBox,        // new, opaque to C++
}

struct AnimatedPlayback {
    current_frame: Property<u32>,
    timer: Timer,
    started_at: Cell<Option<Instant>>,
    paused_elapsed: Cell<Duration>,
    source_tracker: ChangeTracker,   // re-arm when `source` changes
    running_tracker: ChangeTracker,  // start/stop when `running` changes
}
```

`ClippedImage` inherits `ImageItem` in `builtins.slint:427`, and the public
`Image` element *is* `ClippedImage` (`builtins.slint:501`:
`export { ClippedImage as Image }`), so a property and a data box on `ImageItem`
covers both elements with no duplication.

- Full per-element control: two elements, same GIF, one paused, one running,
  different phases — all work.
- Timer lifetime is tied to the item, not the cache.
- Costs: `ImageItem` is `#[repr(C)]` and cbindgen-exported
  (`api/cpp/cbindgen.rs:422`), so this is a C++ ABI change (mitigated by the
  existing opaque-box precedent); `RenderImage` grows a method; and the frame
  index has to travel from the item to the pixels (Q2).

#### Option C — per-item cursor, shared frames, no `render_to_buffer` involvement

Same per-item state as B, but instead of routing through `render_to_buffer`,
each backend matches `ImageInner::AnimatedImage` explicitly and calls
`animated.frame(index)` with the index it got from the item. Maximally explicit,
maximally invasive: all five backends need real arms, including the three that
would otherwise need none.

**Recommendation: B**, with the Q2 mechanism chosen to keep the "three backends
free" property. `running: bool` on `Image` is what `ogoffart` asked for on the
issue, and that is only implementable per-item.

### 5.4 Driving the frame: timer, not tick

Mirror `TextCursorBlinker`:

- On `Item::init` and on every `source`/`running` change (via `ChangeTracker`,
  the `SystemTrayIcon` pattern), recompute whether this item should be playing.
- When it should: record `started_at`, and arm a **single-shot** `Timer` for
  `animated.time_to_next_frame(elapsed)`.
- On fire: recompute `(index, finished) = animated.frame_at(now - started_at)`,
  `current_frame.set(index)`, and if not finished re-arm for the next boundary.

Single-shot-and-re-arm rather than `TimerMode::Repeated` because the per-frame
delays are irregular; recomputing from `now - started_at` rather than
incrementing makes it self-correcting under load.

The rejected alternative is piggybacking on `animation_tick()` /
`AnimationDriver::set_has_active_animations()` (`animations.rs:268, 292`), the way
`examples/sprite-sheet/SpriteSheet.slint` does in userland. That forces a full
repaint at display refresh rate for a GIF whose frames change every 100 ms. It is
simpler to write and it is what a v0 spike would do, but it is strictly worse for
the "decorative looping GIF in a corner" case that is most of #2081.

Caveat to note either way: a `Timer` firing marks the item dirty even if the
window is hidden or the item is scrolled out of view. See Q11.

### 5.5 Getting the frame index to the pixels (the crux)

Given Option B, the item knows the frame index and the backends know how to turn
a `SharedImageBuffer` into their native resource — but `render_to_buffer` sits
between them and has no frame parameter. Four mechanisms, spelled out in Q2.
The one this design assumes for the rest of the document is **Q2-c**: keep
`render_to_buffer`'s signature, and have the item *publish* the frame it wants
onto the shared image via an interior-mutability slot the backends' tracked
closures already read. Concretely:

```rust
// RenderImage gains, with a default so ClippedImage/ImageItem are the only impls:
fn current_frame(self: Pin<&Self>) -> u32 { 0 }
```

and each backend's existing tracked closure (femtovg `:1311`, Skia `:352`,
anyrender `:248`, Qt `:1689`; software has no closure but is wrapped by the
partial renderer's tracker) calls `item.current_frame()` before/while producing
pixels, so the read is registered. The value is threaded into `render_to_buffer`
by a new sibling entry point rather than by mutating shared state:

```rust
pub fn render_frame_to_buffer(
    &self,
    frame: u32,
    target_size_for_scalable_source: Option<Size2D<u32, PhysicalPx>>,
) -> Option<SharedImageBuffer>;

// and render_to_buffer(size) == render_frame_to_buffer(0, size)
```

This keeps every existing `render_to_buffer` caller compiling and correct
(frame 0 for non-animated variants), while the five image draw paths switch to
the frame-aware call. It is a smaller blast radius than changing
`render_to_buffer`'s signature and less magical than shared mutable state.
Q2 lays out the alternatives.

### 5.6 Per-backend work

Assuming Option B + Q2-c:

| Backend | Change |
|---|---|
| **software** (`software/lib.rs:2400-2461`) | Swap `render_to_buffer(size)` → `render_frame_to_buffer(item_frame, size)`. `draw_image_impl` currently receives only `&ImageInner`, so the frame has to be threaded from `draw_image` (`:2765`). No caching to invalidate. **Smallest change of the five.** |
| **Qt** (`qt_window.rs:1639-1743`) | `image_to_pixmap` gains a frame argument. The `ItemCache` closure at `:1689` reads `image.current_frame()`, so the `QPixmap` is rebuilt per frame automatically. No layer-3 cache to key. |
| **femtovg** (`femtovg/images.rs`, `femtovg/itemrenderer.rs:1300`) | `Texture::new_from_image` gains a frame argument (its `_ =>` arm at `:211` then just works). `TextureCacheKey` (`images.rs:228-234`) gains `frame: u32`. The closure at `:1311` reads `item.current_frame()`. **Note**: every frame becomes a distinct GPU texture in `TextureCache`; `drain()` (`images.rs:284-296`) only keeps entries with `strong_count > 1`, so old frames are dropped each frame — meaning a re-upload per frame. Acceptable, but see Q12. |
| **Skia** (`skia/cached_image.rs:31-109`) | New `ImageInner::AnimatedImage` arm calling `image_buffer_to_skia_image(&animated.frame(idx))`. **Must not call `replace_cached_image`** (see §2). `as_skia_image` needs the frame index passed in alongside `target_size_fn`. |
| **anyrender** (`anyrender/itemrenderer.rs:1214-1293`, `imagecache.rs`) | New arm; `ImageVariant` (`imagecache.rs:35-45`) gains `Frame { index: u32 }`. `drain()` (`imagecache.rs:80-93`) already drops unreferenced entries per frame, so stale frames don't accumulate. |

Plus, in core:

- `ImageInner::size()` (`image.rs:541`) — new arm returning the shared frame size.
- `ImageCacheKey::new()` (`image.rs:333`) — new arm returning `a.cache_key()`.
- `ImageInner::PartialEq` (`image.rs:663`) — new arm, `VRc::ptr_eq`.
- `ImageWeightInBytes::weight` (`cache.rs:15`) — new arm.
- `render_to_buffer`/`render_frame_to_buffer` (`image.rs:448`) — new arm.
- `ImageInner::is_svg` — no change (returns false by default).

### 5.7 `.slint` API surface

On `ImageItem` in `builtins.slint` (before `ClippedImage`, so both inherit):

```slint
/// Whether an animated image (GIF, APNG, animated WebP) plays.
/// Setting it to false freezes the current frame; setting it back to true
/// resumes from that frame. Has no effect on static images.
/// \default true
in property <bool> running: true;
```

`in property <bool> ... : true` is already used widely in `builtins.slint`
(e.g. `:840`, `:928`, `:1056`), so a `true` default costs nothing despite
`Property<bool>` defaulting to `false` in Rust.

Deliberately **not** in v1: `frame-count`, `current-frame`, `loop-count`,
`playback-rate`, `finished()` callback, `restart()`. Rationale: `MiKom`'s concern
on #2081 about a dedicated element's API growing without bound applies just as
much to properties on `Image`; each of these is individually defensible and
collectively a different feature. Q6 revisits whether even `running` is the right
minimum.

Rust/C++ API: nothing new is strictly required. `slint::Image` stays opaque and
`Image::size()` keeps working. Q13 asks whether `Image` should gain
`frame_count()` / `is_animated()` accessors.

### 5.8 Memory

`frame_count × width × height × 4` bytes, decoded eagerly. Concrete numbers:

| Example | Frames | Size | Decoded |
|---|---|---|---|
| Typical UI spinner GIF | 20 | 64×64 | 327 KB |
| Typical reaction GIF | 60 | 480×270 | 31 MB |
| 1080p APNG, 5 s @ 30 fps | 150 | 1920×1080 | **1.2 GB** |

The last row is not hypothetical for APNG or animated WebP, which have no
practical size ceiling. Eager decoding without a guard is a memory bomb, so the
design includes a **byte budget check at decode time**: if
`frame_count × w × h × 4` exceeds a threshold, log via `debug_log!` and fall back
to a single-frame `EmbeddedImage` (first frame) rather than allocating. The
threshold value and whether it is configurable is Q4; whether to instead
implement lazy streaming is Q5.

The 5 MiB decode-cache cap needs no change to be *correct* — oversized entries
are already silently not cached (§2) — but it does mean nearly every real
animation is decoded once per `Image` element. Q4 covers whether to raise it.

### 5.9 Cargo features

Proposed: fold into the existing umbrella rather than inventing per-format flags.

```toml
# internal/core/Cargo.toml
animated-images = ["image-decoders", "image?/gif", "image?/png", "image?/webp"]
```

The awkwardness is that `image-default-formats` (`internal/core/Cargo.toml:54`)
is the flag users actually toggle, it is *not* on by default in `slint`
(`api/rs/slint/Cargo.toml:22-31`), and it is an all-or-nothing umbrella over
`image`'s `default-formats`. So today "I want animated GIFs" implies "I also
compile in AVIF, EXR, TIFF, DDS, Farbfeld…". Q8 works through the options.

### 5.10 Threading

Unchanged from the SVG situation and worth restating because animated decode is
the first Slint feature where "decode this off the UI thread" is an obvious ask.
`AnimatedImage` lives in a non-atomic `vtable::VRc`; `slint::Image` documents
itself as `!Send` because of the thread-local caches (`image.rs:780-798`).
Background decoding must therefore produce plain `SharedPixelBuffer`s and be
turned into an `Image` on the UI thread via `invoke_from_event_loop`. No public
API for "construct an animated Image from frames" is proposed in v1 (Q13).

---

## 6. Interactions and things that quietly break

**Compile-time image embedding for the software renderer.**
`internal/compiler/passes/embed_images.rs` converts `@image-url` images into
`EmbeddedResourcesKind::TextureData` (`embedded_resources.rs:102`) — a *single*
`Texture` — for MCU/`no_std` targets. An animated GIF embedded this way is
silently flattened to frame 0 at build time. That is arguably fine (an MCU with
no allocator has nowhere to put 60 frames), but it must be a *documented*
limitation, not a surprise. Q14.

**`ImageInner::StaticTextures` cannot be animated.** Same root cause. Any
animation on the embed-textures path is out of scope, permanently, unless the
compiler learns to emit frame arrays.

**Nine-slice.** `ImageInner::NineSlice` wraps an inner `ImageInner`
(`image.rs:363`). An animated nine-slice would work mechanically (the inner
variant is just delegated to) but is almost certainly not worth testing or
documenting in v1. Recommend: allow it to work if it falls out, don't test it.

**`colorize`.** Every backend colorizes *after* producing the buffer/texture, so
a colorized animated image re-runs colorization per frame. Correct, just slower.
femtovg's `colorize_image` (`femtovg/itemrenderer.rs:~1250-1298`) allocates a new
GPU texture per colorization — per frame, for an animated source. Worth a note in
the docs; not a blocker.

**Tiling.** `ImageTiling` participates in femtovg's `TextureCacheKey` and
anyrender's `ImageVariant::Tile`. Adding `frame` to those keys means an animated
tiled image produces a fresh crop per frame per tile variant. Correct; slow.

**`source-clip-*`.** Works unchanged — the clip is applied to whatever buffer the
frame produced, and all frames are the same size.

**The sprite-sheet example.** `examples/sprite-sheet/SpriteSheet.slint` stays
valid and stays the right answer for uniform-delay sprite sheets. Worth a
cross-reference in the docs so users know which tool to reach for.

---

## 7. Staging

Each stage is independently reviewable and leaves the tree working.

| # | Stage | Depends on | Notes |
|---|---|---|---|
| 1 | `AnimatedImage` type + `ImageInner` variant + `size`/`cache_key`/`PartialEq`/`weight` arms + `render_frame_to_buffer` | — | No behaviour change; nothing constructs the variant yet. Unit-testable in isolation. |
| 2 | Multi-frame decode dispatch in both entry points, behind `animated-images` | 1 | Now `Image::load_from_path("x.gif")` yields an `AnimatedImage`. Every backend shows **frame 0** and nothing regresses, because `render_to_buffer` defaults to frame 0. This is a genuinely shippable intermediate state. |
| 3 | Software renderer frame plumbing | 1, 2 | Smallest backend. Lets the whole thing be exercised end-to-end with `MinimalSoftwareWindow` before touching GPU backends. |
| 4 | `ImageItem.running` + `AnimatedPlaybackBox` + timer driver + `RenderImage::current_frame` | 1–3 | The one genuinely new subsystem. Software renderer animates at the end of this stage. |
| 5 | femtovg + Qt | 4 | Both are "add frame to the existing funnel + key the cache". |
| 6 | Skia + anyrender | 4 | Both need real new match arms. Skia additionally needs the `replace_cached_image` hazard handled. |
| 7 | Docs, examples, screenshot tests, memory-budget guard | 4–6 | |
| 8 | wasm | 4–6 | Separable, and may be deferred indefinitely (§9). |

Stages 5 and 6 are independent of each other and can land in either order or in
parallel.

---

## 8. Testing

**Unit (core).** `AnimatedImage::frame_at` / `time_to_next_frame` against
hand-built delay tables: irregular delays, zero delays, single frame,
`Finite(1)`, `Finite(3)`, `Infinite`, elapsed times beyond the end. Decode tests
against small checked-in fixtures for all three formats plus the negative cases
(non-animated PNG must not become an `AnimatedImage`; non-animated WebP likewise).

**Deterministic rendering.** `tests/screenshots/cases/image/` renders against
software/Skia/anyrender and compares to references in
`tests/screenshots/references/`. Animation needs a way to pin the clock. The
testing backend already drives `update_timers_and_animations()` explicitly
(`internal/backends/testing/testing_backend.rs:243`,
`internal/backends/testing/internal_tests.rs:201`), so the natural approach is to
advance mock time by a known amount and assert the expected frame. This needs the
playback driver to derive its frame from `Instant` rather than from an internal
counter — which §5.1 already requires for other reasons. Q15 asks whether a
test-only "seek to frame N" hook is also warranted.

**Manual.** `tests/manual/` gets a case with several GIFs at different sizes,
one paused, one playing, two elements sharing a source with different `running`
values — the case Option A cannot express, so it doubles as a regression test for
the Q1 decision.

**Memory.** A test that a deliberately oversized animation degrades to a single
frame rather than allocating (the §5.8 guard).

---

## 9. wasm

Today on wasm, `image-decoders` is off ("Not needed on wasm, where the browser
decodes encoded image data", `internal/core/Cargo.toml:52`) and everything goes
through `ImageInner::HTMLImage` wrapping an `HtmlImageElement`
(`internal/core/graphics/image/htmlimage.rs`). `image_mime_type_from_extension`
(`image.rs:1130-1137`) already maps `gif` and `webp` correctly, so the browser
*does* decode and *does* animate the element — but femtovg snapshots it:
`canvas.create_image(&html_image.dom_element, image_flags)`
(`femtovg/images.rs:157`) uploads once into a GL texture cached by
`ImageCacheKey`. The result is a frozen frame.

Three ways out, none free:

- **Re-upload per tick.** Keep the browser as the decoder; add a per-item timer
  (or a plain `animation-tick()` at refresh rate, which on the web is cheap
  because the browser is compositing anyway) that invalidates the texture cache
  entry and re-uploads from the live `<img>`. Needs a frame-agnostic
  invalidation token in `TextureCacheKey`, since the browser owns the frame
  number and we can't observe it. Cheapest, but re-uploads at refresh rate and
  cannot support `running: false` (the `<img>` keeps animating internally).
- **Enable `image-decoders` on wasm** for the animated formats only, and use the
  same native code path. Uniform semantics and per-element control, at a real
  wasm binary-size cost, for a decoder the browser already ships.
- **Ship without wasm support**, document it, revisit. `slintpad` is the main
  wasm consumer.

Recommendation: **defer** (option 3) for v1 and revisit. Q16.

---

## 10. Documentation

- `docs/astro/src/content/docs/reference/elements/` — the `Image` element page
  gains `running`, a supported-formats note, the "which frame is `image-fit`
  applied to" answer (all of them, identically), and the MCU/embed limitation.
- `docs/astro/src/content/docs/reference/property-types/images.mdx` — mention
  that animated formats decode all frames into memory and roughly what that
  costs.
- Cross-reference `examples/sprite-sheet` as the right tool for uniform-delay
  sprite sheets.
- `internal/core/items.rs:9-16` lists the places to keep in sync when adding a
  property to a builtin item — follow it: this module, `builtins.slint`, and the
  docs. (`dynamic_item_tree.rs` and `cbindgen.rs` are listed as "new item only",
  which this is not, but the `AnimatedPlaybackBox` opaque type does need a
  `cbindgen.rs` `pre_body` entry like `SystemTrayIconDataBox` at
  `api/cpp/cbindgen.rs:963-966`.)

---

## 11. Open questions

Ordered roughly by how much downstream work the answer changes.

### Q1 — Per-item or shared playback state?

**Options**: A (shared, on `AnimatedImage`), B (per-item, on `ImageItem`),
C (per-item, backends match explicitly). Detailed in §5.3.

**What hinges on it**: whether `running` can be per-element at all; whether
`ImageItem`'s `#[repr(C)]` layout and the C++ ABI change; whether `RenderImage`
grows a method; roughly a 3× difference in total diff size.

**Recommendation: B.** `ogoffart`'s `running: bool` on `Image` is only meaningful
per-element, and A leaks a live `Timer` into the decode cache. The opaque-data-box
pattern (`FlickableDataBox`, `SystemTrayIconDataBox`) makes the ABI cost routine.

**Counter-argument worth hearing**: A is dramatically smaller and would make
"animated GIFs render and loop" land in maybe a third of the work. If the goal is
to close #2081 fast and iterate, A-then-B is a defensible sequence — but B is not
a strict superset refactor of A, so it would be rework, not extension.

---

### Q2 — How does the frame index reach the pixels?

`render_to_buffer(target_size)` (`image.rs:448`) is the funnel four backends use,
and it has no frame parameter.

**(a) Change the signature** to `render_to_buffer(frame, target_size)`.
Honest, but touches every caller including out-of-tree renderer implementations
if any exist. `render_to_buffer` is `pub` on a `pub` type in `i-slint-core`,
which is an internal crate — so the blast radius is in-tree, but it is a
breaking change to a public-in-crate API.

**(b) Add `render_frame_to_buffer(frame, target_size)`** as a sibling, with
`render_to_buffer(s) == render_frame_to_buffer(0, s)`. Existing callers keep
compiling and stay correct; the five image draw paths opt in. *(This is what §5
assumes.)*

**(c) Interior mutability on the image**: item writes the desired frame into a
`Cell` on `AnimatedImage` before drawing; `render_to_buffer` reads it. No
signature changes at all, but it is order-dependent shared mutable state across
a `VRc` shared by multiple items in one frame — fragile, and it breaks the moment
two items with the same source want different frames within one paint.

**(d) Push the whole decision into the backends** (Option C from Q1): no
`render_to_buffer` involvement, all five backends match explicitly.

**Recommendation: (b).** Smallest correct change; preserves the "software, Qt and
femtovg need no new match arm" property; leaves the door open for Lottie, which
will want `(time, size)` and can add a third sibling.

**Note**: whichever is chosen, `RenderImage` (`item_rendering.rs:340-349`) needs
`fn current_frame(self: Pin<&Self>) -> u32 { 0 }` so the backends can ask the
item. Giving it a default body keeps `RenderImage`'s two impls
(`items/image.rs:148`, `:317`) as the only ones that override it.

---

### Q3 — Timer-driven or tick-driven?

**Timer** (§5.4): wakes only when the frame actually changes; honours irregular
delays exactly; one `Timer` per playing item.

**Tick** (`animation_tick()` + `set_has_active_animations()`): repaints at
display refresh rate whenever anything is animating; simpler; no per-item timer
bookkeeping; wasteful for a 10 fps GIF on a 120 Hz display.

**Recommendation: Timer.** But note two things that make it less clean than it
sounds: (i) an item scrolled off-screen or in a hidden window still fires and
still marks itself dirty (Q11); (ii) N animated images = N timers in the
sorted-vector timer list (`internal/core/timers.rs`), which is fine for a
dashboard and questionable for a list view with 200 animated thumbnails.

**Sub-question**: should there be a shared "animated image driver" that owns one
timer for *all* animated images and dispatches to them, rather than one timer
each? That bounds timer count at 1 and makes global pause trivial, at the cost of
one more piece of thread-local state. Probably worth doing if the list-view case
is considered real.

---

### Q4 — Eager decoding: what is the memory guard?

Eager decode is recommended (matches `ParsedSVG`'s parse-once simplicity, and the
`image` crate's `Frames` iterator is sequential-only so random access needs it
anyway). The question is the guard.

- What threshold? A fixed constant (64 MiB? 128 MiB?), a fraction of the decode
  cache cap, or nothing at all with a documented warning?
- Is it configurable — env var, `SlintContext` setting, Cargo feature?
- What is the fallback when exceeded: first frame as a static image (proposed),
  or refuse to load entirely, or decode a subsampled subset of frames?
- Should the 5 MiB decode-cache cap (`cache.rs:53`) be raised now that a single
  entry can plausibly want 30 MiB? Raising it makes multi-element sharing work;
  leaving it means each element re-decodes. Note the cap is thread-local and
  applies to *all* images, so raising it has effects beyond this feature.

**Recommendation**: a fixed, generous constant (start at 64 MiB) with
`debug_log!` and first-frame fallback; leave the 5 MiB cache cap alone in v1 and
revisit with real numbers. **This is a judgement call I'd want the maintainers'
input on** — it is the one place where the design can produce a user-visible
"my GIF doesn't animate" with no obvious cause.

---

### Q5 — Should lazy/streaming decode be designed for now?

Lazy decoding (keep the encoded bytes, decode a sliding window of frames, restart
the decoder on loop) cuts memory to O(1) in frame count. Against it: GIF frames
are inter-dependent through disposal/blending, so seeking to frame N means
decoding 0..N — random access is out, and a `running: false` → scrub-backwards
interaction would be pathological. It also requires keeping the source bytes
alive, which the current `ImageInner` design does not do for any variant.

**Recommendation**: v1 eager, with the guard from Q4. **But** the choice affects
the `AnimatedImage` struct's shape, so if lazy is ever likely, `frames:
Vec<SharedImageBuffer>` should be behind an accessor from day one — which §5.1
already does (`fn frame(&self, index) -> SharedImageBuffer`). Confirm that this
level of future-proofing is enough, or decide lazy is a non-goal permanently.

---

### Q6 — Is `running: bool` the right v1 API?

`ogoffart` proposed exactly this on #2081, and it is the minimum that is useful.
Questions it leaves open:

- **Resume or restart?** `running: false` then `true` — does it continue from the
  frozen frame (proposed) or start over? Proposed behaviour matches `<video>` and
  is what "pause" means colloquially, but "restart" is what a one-shot
  attention-getting animation usually wants.
- **Is a read-only `frame-count` needed** so a user can build their own scrubber?
  Once `frame-count` exists, `current-frame` as `in-out` follows naturally, and
  then `running` is redundant with it. That is a slippery slope worth either
  taking deliberately or refusing deliberately.
- **Should a finished finite-loop animation notify?** A `finished()` callback is
  the obvious ask and the obvious scope creep.
- **Naming**: `running` vs `playing` vs `paused` (inverted). `running` matches
  `Timer`'s vocabulary in the Rust API; `playing` matches media vocabulary.

**Recommendation**: ship `running` alone, resume-not-restart, no callback. Revisit
after real usage. Flagging it because "just one property" decisions are the ones
that are hardest to change later.

---

### Q7 — Loop-count semantics

The format's loop count is free to read (`AnimationDecoder::loop_count()`), so the
only question is what to do with it.

- **Honour it, or always loop forever?** Honouring it is more correct; looping
  forever is what most UI decoration wants and what a user who picked a GIF off
  the internet probably expects. Browsers honour it.
- **What is shown after a finite loop count is exhausted?** Last frame (GIF
  convention, proposed) or first frame?
- **Does `running = false; running = true` after exhaustion replay?** Under
  "resume" semantics (Q6) it would do nothing, which is probably surprising.
- **Should there be an override property** (`loop-count: int`, `-1` = forever)?
  That is more API surface (see Q6).

**Recommendation**: honour the file's loop count; hold the last frame when
exhausted; no override property in v1; accept that `running` toggling after
exhaustion is a no-op and document it.

---

### Q8 — Cargo feature strategy

Current state: `image-default-formats` (`internal/core/Cargo.toml:54`) is an
all-or-nothing umbrella over `image/default-formats`, and it is **not** in
`slint`'s defaults (`api/rs/slint/Cargo.toml:22-31`). So GIF and WebP don't decode
at all in a default Rust build today.

Options:

- **(a) `animated-images` feature enabling `image?/{gif,png,webp}`** — additive,
  independent of `image-default-formats`, lets someone get animated GIF without
  AVIF/EXR/TIFF. Introduces a second image-format flag, which the prior report
  warned against.
- **(b) No new feature; animation is implicit** whenever the relevant `image`
  format feature is on. Simplest mental model ("if Slint can decode a GIF, it
  animates it"), but no way to opt out of the code size / behaviour, and it
  silently changes behaviour for anyone who already enabled
  `image-default-formats` and is displaying the first frame of a GIF on purpose.
- **(c) Fold into `image-default-formats`** — animation comes with the umbrella.
  No new flag, but ties animation to a flag that is off by default and that also
  pulls in six formats nobody asked for.

**Recommendation: (a).** Explicit, opt-in, doesn't silently change existing
builds, and lets an MCU/`no_std` build exclude the whole frame-vector machinery
cleanly. Needs a corresponding pass-through in `api/rs/slint/Cargo.toml`,
`api/cpp/Cargo.toml`, `internal/interpreter/Cargo.toml`, and the tools.

**Sub-question**: should `animated-images` be in `slint`'s default features? If
not, #2081 is "fixed" only for people who read the feature list.

---

### Q9 — Does a single-frame GIF/APNG/WebP become an `AnimatedImage`?

Uniform (always animated path for GIF) is simpler code and means one less branch.
But it means a static GIF used as an icon becomes an `AnimatedImage` with one
frame, which:

- Skia and anyrender route through their *new* arms rather than the
  well-trodden `EmbeddedImage` arm (losing Skia's `replace_cached_image`
  optimisation, which is real: it caches the `SkImage` in the global decode cache
  so it's shared across items);
- adds a `frame` component to layer-3 cache keys for no reason;
- makes `ImageInner::EmbeddedImage` no longer the answer to "did we decode a
  raster image".

**Recommendation**: if `frame_count == 1`, produce `EmbeddedImage`, not
`AnimatedImage` — for all three formats. Costs one branch at decode time, keeps
the entire static path byte-for-byte unchanged.

---

### Q10 — Frame-delay normalisation

GIFs in the wild routinely specify 0 ms or 10 ms delays, which the format's
original tooling treated as "as fast as possible" but which browsers clamp — the
de-facto rule is *delay < 20 ms becomes 100 ms*. The `image` crate does **not**
apply this; it reports the raw value.

- Apply the browser clamp (matches user expectations, matches every other viewer)?
- Apply a smaller floor (e.g. clamp to one display refresh) to avoid pathological
  timer churn without changing intended timing?
- Apply nothing and be "correct"?

**Recommendation**: apply the browser clamp for GIF specifically (`delay < 20ms →
100ms`), document it, and leave APNG/WebP raw (their tooling doesn't have the
same legacy). Flagging because it is exactly the kind of thing that produces a
"why does my GIF play at the wrong speed in Slint" bug report either way.

---

### Q11 — Should invisible items keep animating?

A `Timer`-driven item that is scrolled out of a `Flickable`, behind another
window, or in a minimized window still fires and still marks itself dirty. The
dirty region machinery means no *pixels* are wasted, but the timer wakeups and
the `SharedImageBuffer` churn are.

- Gate on window visibility (`WindowInner` knows), which is easy and covers the
  minimized/hidden case?
- Gate on the item actually having been drawn last frame (i.e. only re-arm the
  timer from within `draw_image`)? Elegant — self-limiting, no extra state — but
  it means an item that is *never* drawn never starts, and one that stops being
  drawn silently pauses and then jumps on reappearing.
- Do nothing in v1?

**Recommendation**: gate on window visibility only; leave per-item visibility
alone in v1. Note the "re-arm from `draw_image`" idea is genuinely attractive and
worth prototyping — it would make the whole thing self-regulating.

---

### Q12 — femtovg texture churn

Each frame becomes a distinct `TextureCacheKey` entry, and `TextureCache::drain()`
(`femtovg/images.rs:284-296`) drops entries with `strong_count == 1` after each
flush. For an animated image that means: upload frame N, draw, drop, upload
frame N+1… — a GPU upload per displayed frame.

- Accept it (a 480×270 RGBA upload is ~500 KB; at 10 fps that's 5 MB/s, fine on
  desktop, less fine on an embedded GPU)?
- Pre-upload all frames as textures and keep them alive for the animation's
  lifetime (fast, but multiplies GPU memory by frame count on top of the CPU-side
  copy — 60 MB for the reaction-GIF example, on both sides)?
- Exempt animated frames from `drain()` with a small LRU?

**Recommendation**: accept it in v1, measure, revisit. Same question applies to
anyrender's `ImageConversionCache::drain()` (`imagecache.rs:80-93`).

---

### Q13 — Public Rust/C++ API for animated images?

Nothing is strictly required. But:

- Should `slint::Image` gain `is_animated()` / `frame_count()`?
- Should there be a constructor to build an animated `Image` from a
  `Vec<(SharedPixelBuffer, Duration)>` — i.e. can an application supply frames
  programmatically, the way it can supply a static buffer today
  (`Image::from_rgba8`)? That is a real ask for anyone decoding video or
  generating frames.
- `Image::to_rgba8()` — which frame does it return for an animated image? First,
  or currently-displayed (which isn't well-defined off the item)? Proposed:
  first frame, documented.

**Recommendation**: none of it in v1 except documenting `to_rgba8()`'s behaviour.
Flagging the programmatic-frames constructor because it is the natural next
request and the `AnimatedImage` struct shape should not preclude it.

---

### Q14 — MCU / `no_std` / compile-time embedding

An `@image-url` GIF processed by `embed_images.rs` for the software renderer
becomes a single `Texture` — frame 0, silently. Options:

- Document it as a limitation and move on.
- Emit a compiler **warning** when an animated source is embedded as a texture,
  so the user knows their animation was flattened.
- Teach the compiler to emit a frame array (large scope, questionable value on a
  device with no allocator).

**Recommendation**: warning + documentation. The warning is cheap
(`embed_images.rs` already has the decoded image in hand) and turns a silent
surprise into a build-time notice.

---

### Q15 — Test-time determinism hook

Screenshot tests need "render frame N of this GIF, deterministically". The
testing backend already controls the clock via `update_timers_and_animations()`.
Is mock-clock advancement sufficient, or is an explicit test-only API needed
(e.g. `i-slint-backend-testing` exposing "set the animated-image clock to T")?

**Recommendation**: try mock-clock first — §5.1's elapsed-time-derived frame index
should make it work — and add an explicit hook only if the tests turn out flaky.
Deciding this early matters because it constrains whether the frame index may
ever be a plain incrementing counter (it may not).

---

### Q16 — wasm

Defer, re-upload-per-tick, or ship native decoders to wasm? §9 lays out the
trade-offs. **Recommendation: defer**, and document that animated images show a
single frame on the web. Worth an explicit maintainer decision because `slintpad`
is the shop window and a frozen GIF there looks like a bug.

---

## 12. What this design deliberately does not do

- No Lottie. Different `ImageInner` shape, different API surface, different
  dependency story (#5549).
- No video. WebM/MP4 need a demuxer, audio sync, and hardware decode paths; the
  `Image` element is the wrong home for that.
- No frame-level API (`current-frame`, `frame-count`, scrubbing).
- No `finished()` callback or animation events.
- No playback rate, direction, or markers.
- No animated `StaticTextures` (compile-time embedded, MCU).
- No off-thread decoding API.

Each of these is a separate, defensible feature. The v1 boundary is "an animated
image file, dropped into an `Image` element, plays; and you can pause it".
