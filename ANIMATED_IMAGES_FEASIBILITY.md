# Feasibility report: animated images (#2081) and Lottie animations (#5549)

## TL;DR

Both features are feasible, and both can follow the architectural shape already
established by SVG support (`ImageInner::Svg`). The fit is good but not
identical for the two issues:

- **GIF / APNG / WebP (#2081)** is the easier of the two. The `image` crate
  Slint already depends on can decode all three formats frame-by-frame today
  (`AnimationDecoder::into_frames()`), so no new external dependency is
  needed. The main new work is a frame/time-driven playback mechanism, which
  has ready analogues in Slint's existing `Timer` and per-item render-cache
  infrastructure.
- **Lottie (#5549)** can also be modeled as "SVG plus a time axis" *if* a
  raster-producing Lottie renderer is used (e.g. a ThorVG binding such as
  `rlottie`/`dotlottie-rs`). That keeps it uniform across every Slint renderer
  backend, exactly mirroring how `ParsedSVG::render()` works today, but it
  pulls in a C++ rendering engine as a new dependency. The alternative,
  `velato` (pure Rust, renders into a `vello::Scene`), avoids the raster
  round-trip and the C++ dependency, but only integrates naturally with the
  `anyrender`/Vello-based renderer, not with femtovg/Skia/software/Qt.

Recommended sequencing: implement #2081 first (it's pure Rust, exercises the
new playback-driver plumbing without any new native dependency), then layer
Lottie on top of that same plumbing.

The rest of this document lays out the current architecture, then a concrete
design for each feature built on top of it.

---

## 1. How SVG support is actually structured today

SVG support is not one component, it is three cooperating pieces, and the
"do it like SVG" question really means "do the same three pieces exist for
the new format?"

### 1.1 A dedicated, size-agnostic `ImageInner` variant

`internal/core/graphics/image.rs:420-439`:

```rust
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
    WGPUTexture(WGPUTexture) = 8,
}
```

`Svg` wraps a `ParsedSVG` (`internal/core/graphics/image/svg.rs:10-14`):

```rust
pub struct ParsedSVG {
    svg_tree: usvg::Tree,
    cache_key: ImageCacheKey,
    weight_in_bytes: usize,
}
```

Note what it *doesn't* store: no rendered bitmap, no per-size cache. It's
purely the parsed, resolution-independent representation. `weight_in_bytes`
exists only to charge Slint's decode-level LRU cache (§1.4).

### 1.2 A stateless "give me pixels" call, not a cache

`ParsedSVG::render()` (`svg.rs:57-89`) rasterizes fresh, every time it's
called:

```rust
pub fn render(&self, size: Option<Size2D<u32, PhysicalPx>>) -> Result<SharedImageBuffer, usvg::Error> {
    let tree = &self.svg_tree;
    let (target_size, transform) = /* compute scale to `size` */;
    let mut buffer = SharedPixelBuffer::new(target_size.width(), target_size.height());
    let mut skia_buffer = tiny_skia::PixmapMut::from_bytes(buffer.make_mut_bytes(), ...)?;
    resvg::render(tree, transform, &mut skia_buffer);
    Ok(SharedImageBuffer::RGBA8Premultiplied(buffer))
}
```

`ImageInner::render_to_buffer(&self, target_size: Option<Size2D<u32, PhysicalPx>>)`
(`image.rs:448`) is the type-erased entry point every renderer backend calls;
for the `Svg` variant it just forwards to `ParsedSVG::render()`.

### 1.3 Per-backend, size-keyed caching around that call

Because rendering is stateless and not cheap, every backend wraps it in its
own cache, all following the same shape — "cache the last bitmap for this
`(ImageCacheKey, target_size, ...)` combination, invalidate when the item's
properties change":

- **femtovg** (`internal/renderers/femtovg/images.rs:228-301`): a
  `TextureCache<TextureCacheKey>` where
  `TextureCacheKey { source_key: ImageCacheKey, target_size_for_scalable_source: Option<Size2D<u32,PhysicalPx>>, gpu_image_flags, gpu_image_tiling }`.
- **anyrender** (`internal/renderers/anyrender/imagecache.rs`): an
  `ImageConversionCache` keyed by `(ImageCacheKey, ImageVariant::Sized{w,h})`,
  explicitly commented as mirroring femtovg's cache.
- **Skia** and the **software renderer** lean on the generic per-item cache
  `ItemCache<T>` (`internal/core/item_rendering.rs:27-140`, a
  `RefCell<HashMap<component_ptr, HashMap<item_index, CachedGraphicsData<T>>>>`
  paired with a `PropertyTracker`). The closure that (re)computes the cached
  value reads `item.target_size()` / `item.source()`, so a change to either
  property automatically invalidates the entry. The software renderer's SVG
  path actually re-rasterizes on every single frame with no caching at all —
  a pre-existing gap worth knowing about since animated images stress this
  exact code path much harder than a static SVG does.

The call chain for the femtovg backend, end to end:

```
ImageItem::render
  -> ItemRenderer::draw_image                     (item_rendering.rs:471, trait)
    -> femtovg draw_image_impl                     (itemrenderer.rs:1300)
      -> ItemCache::get_or_update_cache_entry      (per item, PropertyTracker-gated)
        -> Texture::new_from_image                 (images.rs:131)
          -> ImageInner::render_to_buffer          (image.rs:448)
            -> ParsedSVG::render                    (svg.rs:57) -> resvg::render(...)
```

Skia and anyrender call `ParsedSVG::render()` even more directly
(`internal/renderers/skia/cached_image.rs:69`,
`internal/renderers/anyrender/itemrenderer.rs:1231`).

### 1.4 A separate, decode-level cache upstream of all of this

`internal/core/graphics/image/cache.rs` has a thread-local
`clru::CLruCache<ImageCacheKey, ImageInner, ..., ImageWeightInBytes>` capped
at **5 MiB**, keyed by path+mtime / embedded-data pointer / URL. This caches
the *decoded* `ImageInner` (e.g. the parsed `usvg::Tree`) so repeatedly
constructing `Image::load_from_path("x.svg")` doesn't re-parse the file. This
is a different cache from the per-size bitmap caches in §1.3, and it's
important for the effort estimate: **animated formats will blow through 5 MiB
very quickly** once frames are pre-decoded (see §2.4), so this cap needs
revisiting.

### 1.5 Decode dispatch and the raster path's actual limitation

Format dispatch (`ImageInner::load_from_data_with_cache_key`,
`image.rs:566-632`) special-cases SVG by extension/content-sniffing, then
falls through to the `image` crate:

```rust
let maybe_image = if let Some(format) = format {
    image::load_from_memory_with_format(data.as_slice(), format)
} else {
    image::load_from_memory(data.as_slice())
};
```

`image::load_from_memory[_with_format]` returns a single `DynamicImage` —
**only the first frame of a GIF/animated-PNG/animated-WebP is ever decoded**
today. This is the concrete reason #2081 doesn't work: the plumbing to reach
a *single* frame exists, the plumbing to reach *all* frames with their
timing does not.

Feature-flag pattern for reference (`internal/core/Cargo.toml:51-55`):

```toml
image-decoders = ["dep:image", "dep:clru"]
image-default-formats = ["image?/default-formats"]
svg = ["dep:resvg", "i-slint-common/svg-text"]
```

`svg` is always-on transitively (every backend/renderer Cargo.toml enables it
unconditionally); `image-default-formats` is the one flag end users toggle
via `api/rs/slint/Cargo.toml`, and it's an umbrella over the `image` crate's
own `default-formats` (which already includes GIF/WebP/etc. *static*
decoding). There is no precedent for one Cargo feature per raster format —
new work should probably follow the umbrella pattern rather than invent
`gif`/`webp`/`apng` as three separate flags.

### 1.6 Timer / animation infrastructure already available

Two mechanisms already drive per-frame runtime behavior and both are
directly reusable:

- **`AnimationDriver`** (`internal/core/animations.rs:231-292`): a thread-local
  driver with a `global_instant: Property<Instant>`. Evaluating the builtin
  `animation-tick()` both reads the current tick *and* calls
  `set_has_active_animations()`, which is what makes the windowing backend
  keep requesting redraws (see `winit/event_loop.rs:642-651`). This is a
  continuous, "redraw every frame while something is playing" mechanism.
- **`Timer`** (`internal/core/timers.rs:62-120`, driven from
  `platform.rs:289` `update_timers_and_animations()` every event-loop
  iteration): supports one-shot and repeating timers with arbitrary
  intervals — a much better fit for GIF/APNG/WebP, whose per-frame delays are
  irregular (stored per-frame in centiseconds), than a fixed-period
  continuous tick.

Telling proof that the *concept* of frame-driven image playback is already
in demand but has no first-class support: the shipped
`examples/sprite-sheet/SpriteSheet.slint` example implements sprite-sheet
animation entirely in userland, using `ClippedImage`'s `source-clip-*`
properties plus `animation-tick()`:

```slint
property <int> current-frame: playing
    ? (total-frames * (animation-tick() / duration)).mod(total-frames)
    : frame.mod(total-frames).abs();
sheet := Image {
    source: root.source;
    source-clip-x: self.source-clip-width * current-frame.mod(root.frames-wide);
    source-clip-y: self.source-clip-height * (current-frame / root.frames-wide).floor();
};
```

This is good validation that the primitives compose, but it only works
because a sprite sheet's frame duration is *uniform*. GIF/APNG/WebP/Lottie
all need variable per-frame timing, which this pattern can't express.

### 1.7 Threading constraint that carries over unchanged

`ParsedSVG` lives inside a `vtable::VRc` (non-atomic `Rc`), and the public
`Image` type documents itself as not `Send` specifically because of
thread-local caches (`image.rs:780-798`). Any animated-image design inherits
this constraint: decode/rasterize work that must happen off the UI thread has
to produce plain `SharedPixelBuffer`s and be turned into an `Image` back on
the UI thread via `slint::invoke_from_event_loop`, exactly as documented for
the existing image loading API.

---

## 2. Proposed design: GIF / APNG / WebP (#2081)

### 2.1 New `ImageInner` variant

```rust
#[cfg(feature = "animated-images")]
AnimatedImage(vtable::VRc<OpaqueImageVTable, animated::AnimatedRasterImage>) = 9,
```

`AnimatedRasterImage` holds the decoded frames plus timing:

```rust
pub struct AnimatedRasterImage {
    frames: Vec<SharedImageBuffer>,      // pre-decoded, one per frame
    frame_delays: Vec<Duration>,         // per-frame delay, same length as `frames`
    loop_count: LoopCount,               // Infinite | Finite(u32)
    cache_key: ImageCacheKey,
}
```

This is actually *simpler* than `ParsedSVG` in one respect: raster frames
already have a fixed pixel resolution, so there's no "rasterize for this
target size" step — scaling to the item's on-screen size is handled the same
way `EmbeddedImage` scaling is handled today (by the existing fit/scale code
in each backend's draw path), not by `ImageInner` itself. The only new axis
`ImageInner` needs to expose is *which frame*, not *what size*.

### 2.2 Decode dispatch

The `image` crate (already pinned at `0.25` in the workspace root
`Cargo.toml`) implements `AnimationDecoder<'a>` with `into_frames()` for:

| Format | Decoder | Cargo feature |
|---|---|---|
| GIF | `GifDecoder` | `gif` |
| Animated PNG | `ApngDecoder` | `png` |
| Animated WebP | `WebPDecoder` | `webp` |

So the decode side is close to a drop-in change to
`load_from_data_with_cache_key` (`image.rs:566-632`): instead of always
taking `image::load_from_memory(...)`'s single `DynamicImage`, detect
multi-frame sources and use `into_frames()` instead. Two format-specific
wrinkles:

- **GIF**: every GIF can go through the animated path uniformly — a
  "static" GIF is just one frame with an infinite delay, so there's no need
  for the dual-path branching the other two formats require.
- **PNG**: a `.png` file must first be checked for an `acTL` chunk to know
  whether it's animated at all (`ApngDecoder` vs. plain `PngDecoder`); most
  `.png` files are not APNG, so this needs a cheap up-front check, not an
  unconditional attempt to treat every PNG as animated.
- **WebP**: same idea — static vs. animated WebP is distinguished by an
  `ANIM` chunk; needs the equivalent check before choosing `WebPDecoder`'s
  animated path.

### 2.3 The actual new piece of work: a playback driver

This is where the design differs most from SVG, because SVG has no time
axis at all. Two viable approaches, in order of recommendation:

**A. Timer-driven frame advance (recommended).** Each `Image` item whose
`source` resolves to an `AnimatedImage` and is "running" owns a small piece
of per-item playback state — current frame index, plus a `Timer` armed for
exactly `frame_delays[current_frame]`. When it fires, advance the index
(wrapping according to `loop_count`), mark the item dirty, and re-arm for the
next frame's delay. This uses `Timer` exactly as designed
(`internal/core/timers.rs`), only wakes the event loop when the visible frame
actually needs to change, and honors irregular per-frame delays exactly.

**B. Continuous-tick, elapsed-time driven (simpler, less efficient).**
Piggyback on `AnimationDriver`/`animation-tick()` the way the sprite-sheet
example does: on every redraw, compute elapsed time since playback started,
walk (or binary-search) the cumulative delay table to find the active frame,
and call `AnimationDriver::set_has_active_animations()` to keep the window
repainting continuously while any animated image is playing. Simpler to
implement (no per-item `Timer` bookkeeping), but it redraws at full display
refresh rate (typically 60 Hz+) even for a GIF whose frames only change every
100 ms, which is pure wasted work — the same class of inefficiency that
already exists for the sprite-sheet userland example.

Recommendation: **A**, since `Timer` already exists for exactly this
purpose, and the efficiency gap versus B is real for the common "decorative
looping GIF" use case.

Where should this per-item state actually live? Not inside the shared,
cached `AnimatedRasterImage` — that's decode-cache data, potentially shared
by multiple `Image` elements pointing at the same file, and two such
elements should be able to show different frames (e.g. one paused, one
playing, or started at different times). This is exactly the same shape as
the per-item caches already used by Skia/software (`ItemCache<T>` in
`internal/core/item_rendering.rs`) — playback position should be stored the
same way, keyed by `(component_ptr, item_index)`, alongside (or as an
extension of) `CachedRenderingData`.

### 2.4 Memory: a real caveat, not just a footnote

SVG's decode cache stores one small vector tree per image. Pre-decoding every
frame of an animated image up front means memory scales with
`frame_count × width × height × 4 bytes` — a handful of GIFs can easily
exceed the current 5 MiB decode-cache cap (`cache.rs`) by itself. Two options:

- Keep eager "decode all frames once" (matches `ParsedSVG`'s "parse once"
  simplicity, easiest to implement first) and explicitly re-tune or bypass
  the 5 MiB cap for this variant, documenting the memory tradeoff.
- Decode lazily, keeping only a small ring of recently-shown frames, using
  `image`'s `Frames<'a>` as a genuinely streaming iterator and re-opening/
  restarting the decoder on each loop. Lower memory, more CPU on loop restart,
  and requires keeping the source bytes (or a re-openable reader) alive for
  the lifetime of the image, which is a bigger structural change.

Recommendation: ship eager decoding first (smaller, matches existing
patterns), flag the cache-sizing issue explicitly rather than silently
shipping a foot-gun, and leave lazy streaming as a follow-up if real-world
memory pressure shows up.

### 2.5 API surface

Per `ogoffart`'s own comment on #2081, add a `running: bool` property
(default `true`) directly to `Image`, rather than a new element — this
matches the existing element's `image-fit`/`colorize`/etc. property style and
avoids `MiKom`'s concern about a dedicated `AnimatedImage` element's API
surface growing unboundedly, since v1 scope here is deliberately minimal
(just play/pause). Loop count: default to "loop forever" for v1 (the common
UI-decoration use case), honoring the format's own loop-count metadata is a
reasonable v1.1, not blocking.

### 2.6 Per-backend wiring

Each of the 5 call sites identified for SVG (`software`, `femtovg`, `skia`,
`anyrender`, `qt`) needs a new match arm for `ImageInner::AnimatedImage`,
almost all of which is mechanical: pick the current frame's
`SharedImageBuffer` (already decoded — no rasterization needed, unlike SVG)
and feed it through the exact same upload/cache path already used for
`EmbeddedImage`. The only backend-specific addition is extending each cache
key (e.g. femtovg's `TextureCacheKey`) with the current frame index, mirroring
how it already includes `target_size_for_scalable_source`.

One Qt-specific opportunity worth flagging (not verified in this pass — the
Qt backend's `draw_image` in `internal/backends/qt/qt_window.rs` was not
traced in detail): Qt has native `QMovie` support for animated GIFs, which
could let the Qt backend get GIF playback almost for free instead of going
through the `image`-crate decode path — worth a spike before assuming the
generic path is the only option there.

### 2.7 wasm

`image_mime_type_from_extension` (`image.rs:1137`) already maps `gif` →
`image/gif` and `webp` → `image/webp`, so `HTMLImage` already picks the right
MIME type. What's unverified is whether Slint's wasm path draws the live
`<img>` element directly (in which case the browser's native GIF/WebP looping
might work with little extra effort) or snapshots it to a static texture once
(in which case wasm needs the same explicit frame-driving logic as native).
This should be checked early since it changes the wasm effort estimate
significantly in either direction.

---

## 3. Proposed design: Lottie (#5549)

The key architectural fork for Lottie is which renderer to build on, because
it determines whether "similarly to SVG" (the premise of this report) is
actually achievable uniformly across backends.

### 3.1 Two families of Lottie renderer

| | Raster (e.g. `rlottie` / `dotlottie-rs`, both ThorVG-based) | Vector (`velato`) |
|---|---|---|
| Output | RGBA pixel buffer per requested `(time, size)` | `vello::Scene` (vector) |
| Backend fit | **All** Slint renderers (software, femtovg, Skia, anyrender, Qt) — same shape as `ParsedSVG::render()` | Only `anyrender` (Vello-based) |
| New dependency | ThorVG, a C++ library (bound via `rlottie`/`dotlottie-rs`) | Pure Rust, but Vello-only |
| Quality/perf ceiling | Rasterized at a fixed size, like SVG | Resolution-independent, GPU vector — potentially better |

The raster family is the one that actually matches "implemented similarly to
how SVG is implemented" — and the match is unusually exact:

```rust
// ParsedSVG, today:
pub fn render(&self, size: Option<Size2D<u32, PhysicalPx>>) -> Result<SharedImageBuffer, usvg::Error>;

// Proposed ParsedLottie:
pub fn render(&self, time: Duration, size: Option<Size2D<u32, PhysicalPx>>) -> Result<SharedImageBuffer, LottieError>;
```

Lottie's own model is "parse once into an in-memory composition, then
rasterize on demand for a given time and size" — i.e. it needs *both* of the
axes SVG and GIF each need one of (SVG: size only; GIF: frame/time only,
fixed size). This means the `ImageInner::Lottie` variant looks much more like
`ImageInner::Svg` (stateless, parse-once, render-on-demand struct) than like
the GIF design's `AnimatedRasterImage` (pre-decoded frame list) — it should
reuse the *SVG* per-backend caching pattern (§1.3) extended with a time
component, plus the GIF design's Timer-based playback driver (§2.3) to decide
*when* to ask for a new time/frame.

Recommendation: build Lottie on the raster family (`rlottie`/`dotlottie-rs`)
first, specifically because it slots into every existing backend with the
same shape SVG already uses, matching the user's premise directly and
avoiding an anyrender-only feature. `velato`/Vello-native rendering is worth
revisiting later purely as a quality/perf upgrade path for the anyrender
backend specifically (in the same spirit as a backend someday
special-casing SVG rendering for its own renderer), not as the initial
implementation.

### 3.2 API surface is a bigger jump than GIF/APNG/WebP

Per `tronical`'s comment on #5549, Lottie wants play/pause, direction,
named/frame markers ("go to marker X"), and possibly playback-rate/quality
overrides — a materially richer control surface than "play or don't." This
doesn't fit comfortably as a couple of extra properties on `Image` the way
`running` does for GIF. This is a genuine open design question the team
already flagged in `2081`'s own thread (`MiKom`'s comment about a dedicated
element's API growing) — it's worth treating Lottie's public API as a
separate decision from GIF/APNG/WebP's, even though the underlying
`ImageInner` plumbing can be shared.

### 3.3 Build/dependency concerns

ThorVG (via `rlottie`/`dotlottie-rs`) is a C++ library. Slint already accepts
C++ dependencies for the Qt and Skia backends, so this isn't unprecedented,
but SVG's `resvg`/`usvg`/`tiny-skia` stack is pure Rust and always-on by
default — Lottie should not follow that precedent. It should be an
opt-in, off-by-default feature (its own `lottie` Cargo feature, not folded
into `svg` or `animated-images`), with the C++ toolchain requirement
documented, especially given Slint also targets bare-metal/MCU builds via the
`no_std` software renderer where pulling in a C++ rendering engine may not be
viable at all.

---

## 4. Summary and sequencing

| Stage | New native dependency | Reuses existing infra | Relative effort |
|---|---|---|---|
| `ImageInner::AnimatedImage` + `image`-crate multi-frame decode dispatch | None (already a dependency) | Decode-dispatch pattern (§1.7/§2.2) | Small–medium |
| Timer-based playback driver + per-item frame state | None | `Timer` (§1.6), `ItemCache<T>` (§1.3) | Medium (the one genuinely new piece) |
| Per-backend wiring (5 call sites) | None | SVG's existing match-arm-per-backend structure | Medium, mostly mechanical |
| `Image.running` + loop semantics + docs/examples | None | — | Small |
| Lottie `ImageInner::Lottie` + raster renderer binding | ThorVG (C++, via `rlottie`/`dotlottie-rs`) | SVG's stateless-render-on-demand shape (§1.2) + the playback driver above | Small–medium, *after* the GIF stage lands |
| Lottie control-surface API (markers, direction, rate) | — | — | Separate design discussion, larger/open-ended |

Both features are architecturally sound to build "like SVG," with GIF/APNG/
WebP being close to a textbook fit (the missing piece is purely the
frame-timing driver, which existing `Timer` infrastructure covers well) and
Lottie being an even closer conceptual fit (stateless render-on-demand,
exactly like SVG but parameterized by time as well as size) provided a
raster-producing renderer is chosen over a Vello-only vector one.
