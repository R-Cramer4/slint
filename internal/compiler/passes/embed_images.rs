// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use crate::EmbedResourcesKind;
use crate::diagnostics::BuildDiagnostics;
use crate::embedded_resources::*;
use crate::expression_tree::{Expression, ImageReference};
use crate::object_tree::*;
#[cfg(feature = "renderer-software")]
use image::GenericImageView;
use smol_str::SmolStr;
use std::cell::RefCell;
use std::collections::HashMap;
use typed_index_collections::TiVec;
use url::Url;

/// The fonts shared with `embed_glyphs` to rasterize SVG `<text>`. Only the
/// software renderer embeds textures, so elsewhere this is an unused placeholder.
#[cfg(feature = "renderer-software")]
pub(crate) type SharedFontCollection = super::embed_glyphs::SharedFontCollection;
#[cfg(not(feature = "renderer-software"))]
pub(crate) type SharedFontCollection = ();

pub async fn embed_images(
    doc: &Document,
    embed_files: EmbedResourcesKind,
    scale_factor: f32,
    resource_url_mapper: &Option<crate::ResourceUrlMapper>,
    font_collection: Option<&SharedFontCollection>,
    diag: &mut BuildDiagnostics,
) {
    if embed_files == EmbedResourcesKind::Nothing && resource_url_mapper.is_none() {
        return;
    }

    let global_embedded_resources = &doc.embedded_file_resources;
    let mut path_to_id = HashMap::<SmolStr, EmbeddedResourcesIdx>::new();

    let mut all_components = Vec::new();
    doc.visit_all_used_components(|c| all_components.push(c.clone()));
    let all_components = all_components;

    let mapped_urls = {
        let mut urls = HashMap::<Url, Option<Url>>::new();

        if let Some(mapper) = resource_url_mapper {
            // Collect URLs (sync!):
            for component in &all_components {
                visit_all_expressions(component, |e, _| {
                    collect_image_urls_from_expression(e, &mut urls)
                });
            }

            // Map URLs (async -- well, not really):
            for (url, mapped) in urls.iter_mut() {
                *mapped = (*mapper)(url).await;
            }
        }

        urls
    };

    // Use URLs (sync!):
    for component in &all_components {
        visit_all_expressions(component, |e, _| {
            embed_images_from_expression(
                e,
                &mapped_urls,
                global_embedded_resources,
                &mut path_to_id,
                embed_files,
                scale_factor,
                diag,
                font_collection,
            )
        });
    }
}

/// The URL handed to the resource mapper, and the key of the mapped-resource
/// map, for a reference the mapper may rewrite. A local [`ImageReference::Path`]
/// becomes a `file://` URL; an [`ImageReference::Url`] is used as-is. Everything
/// else (`data:` URIs, already-embedded references) returns `None` and is left
/// untouched.
fn reference_mapper_url(resource_ref: &ImageReference) -> Option<Url> {
    match resource_ref {
        ImageReference::Url(url) => Some(url.clone()),
        ImageReference::Path(path) => {
            // `Url::from_file_path` is absent on `wasm32-unknown-unknown`, which
            // only ever sees URL references and so never reaches this branch.
            #[cfg(not(target_arch = "wasm32"))]
            {
                Url::from_file_path(path).ok()
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = path;
                None
            }
        }
        _ => None,
    }
}

fn collect_image_urls_from_expression(e: &Expression, urls: &mut HashMap<Url, Option<Url>>) {
    if let Expression::ImageReference { resource_ref, .. } = e
        && let Some(url) = reference_mapper_url(resource_ref)
    {
        urls.insert(url, None);
    };

    e.visit(|e| collect_image_urls_from_expression(e, urls));
}

fn embed_images_from_expression(
    e: &mut Expression,
    urls: &HashMap<Url, Option<Url>>,
    global_embedded_resources: &RefCell<TiVec<EmbeddedResourcesIdx, EmbeddedResources>>,
    path_to_id: &mut HashMap<SmolStr, EmbeddedResourcesIdx>,
    embed_files: EmbedResourcesKind,
    scale_factor: f32,
    diag: &mut BuildDiagnostics,
    font_collection: Option<&SharedFontCollection>,
) {
    if let Expression::ImageReference { resource_ref, source_location, nine_slice: _ } = e {
        // Apply the resource mapper. A Path/Url may be replaced with the mapped
        // URL (e.g. a `data:` URL), so re-classify the reference.
        if let Some(url) = reference_mapper_url(resource_ref)
            && let Some(mapped) = urls.get(&url).cloned().flatten()
        {
            *resource_ref = ImageReference::from_mapped_url(mapped);
        }

        match resource_ref {
            ImageReference::DataUri(data) => {
                // Data URIs have no external file to track, so skip for
                // Nothing (interpreter) and ListAllResources (dependency tracking).
                if !matches!(
                    embed_files,
                    EmbedResourcesKind::Nothing | EmbedResourcesKind::ListAllResources
                ) {
                    let image_ref = embed_data_uri(
                        global_embedded_resources,
                        path_to_id,
                        data,
                        embed_files,
                        scale_factor,
                        diag,
                        source_location,
                        font_collection,
                    );
                    *resource_ref = image_ref;
                }
            }
            ImageReference::Path(_) | ImageReference::Url(_) => {
                let is_builtin = matches!(
                    resource_ref,
                    ImageReference::Url(url) if url.scheme() == "builtin"
                );
                if embed_files != EmbedResourcesKind::Nothing
                    && (embed_files != EmbedResourcesKind::OnlyBuiltinResources || is_builtin)
                {
                    let path = resource_ref.source().expect("Path/Url have a source");
                    let image_ref = embed_image(
                        global_embedded_resources,
                        path_to_id,
                        embed_files,
                        path,
                        scale_factor,
                        diag,
                        source_location,
                        font_collection,
                    );
                    if embed_files != EmbedResourcesKind::ListAllResources {
                        *resource_ref = image_ref;
                    }
                }
            }
            ImageReference::None
            | ImageReference::EmbeddedData { .. }
            | ImageReference::EmbeddedTexture { .. } => {}
        }
    };

    e.visit_mut(|e| {
        embed_images_from_expression(
            e,
            urls,
            global_embedded_resources,
            path_to_id,
            embed_files,
            scale_factor,
            diag,
            font_collection,
        )
    });
}

fn embed_image(
    global_embedded_resources: &RefCell<TiVec<EmbeddedResourcesIdx, EmbeddedResources>>,
    path_to_id: &mut HashMap<SmolStr, EmbeddedResourcesIdx>,
    embed_files: EmbedResourcesKind,
    path: &str,
    _scale_factor: f32,
    diag: &mut BuildDiagnostics,
    source_location: &Option<crate::diagnostics::SourceLocation>,
    _font_collection: Option<&SharedFontCollection>,
) -> ImageReference {
    let extension = || {
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|x| x.to_string())
            .unwrap_or_default()
    };

    if let Some(&resource_id) = path_to_id.get(path) {
        return match global_embedded_resources.borrow()[resource_id].kind {
            #[cfg(feature = "renderer-software")]
            EmbeddedResourcesKind::TextureData { .. } => {
                ImageReference::EmbeddedTexture { resource_id }
            }
            _ => ImageReference::EmbeddedData { resource_id, extension: extension() },
        };
    }

    let mut resources = global_embedded_resources.borrow_mut();
    let mut push = |kind| {
        let id = resources.push_and_get_key(EmbeddedResources { path: Some(path.into()), kind });
        path_to_id.insert(path.into(), id);
        id
    };

    if embed_files == EmbedResourcesKind::ListAllResources {
        push(EmbeddedResourcesKind::ListOnly);
        return ImageReference::None;
    }

    let Some(_file) = crate::fileaccess::load_file(std::path::Path::new(path)) else {
        diag.push_error(format!("Cannot find image file {path}"), source_location);
        return ImageReference::None;
    };

    #[cfg(feature = "renderer-software")]
    if embed_files == EmbedResourcesKind::EmbedTextures {
        return match load_image(_file, _scale_factor, _font_collection, path, diag, source_location)
        {
            Ok((img, source_format, original_size)) => {
                let resource_id = push(EmbeddedResourcesKind::TextureData(generate_texture(
                    img,
                    source_format,
                    original_size,
                )));
                ImageReference::EmbeddedTexture { resource_id }
            }
            Err(err) => {
                diag.push_error(format!("Cannot load image file {path}: {err}"), source_location);
                ImageReference::None
            }
        };
    }

    let resource_id = push(EmbeddedResourcesKind::FileData);
    ImageReference::EmbeddedData { resource_id, extension: extension() }
}

#[cfg(feature = "renderer-software")]
trait Pixel {
    //fn alpha(&self) -> f32;
    //fn rgb(&self) -> (u8, u8, u8);
    fn is_transparent(&self) -> bool;
}
#[cfg(feature = "renderer-software")]
impl Pixel for image::Rgba<u8> {
    /*fn alpha(&self) -> f32 { self[3] as f32 / 255. }
    fn rgb(&self) -> (u8, u8, u8) { (self[0], self[1], self[2]) }*/
    fn is_transparent(&self) -> bool {
        self[3] <= 1
    }
}

#[cfg(feature = "renderer-software")]
fn generate_texture(
    image: image::RgbaImage,
    source_format: SourceFormat,
    original_size: Size,
) -> Texture {
    // Analyze each pixels
    let mut top = 0;
    let is_line_transparent = |y| {
        for x in 0..image.width() {
            if !image.get_pixel(x, y).is_transparent() {
                return false;
            }
        }
        true
    };
    while top < image.height() && is_line_transparent(top) {
        top += 1;
    }
    if top == image.height() {
        return Texture::new_empty();
    }
    let mut bottom = image.height() - 1;
    while is_line_transparent(bottom) {
        bottom -= 1;
        assert!(bottom > top); // otherwise we would have a transparent image
    }
    let is_column_transparent = |x| {
        for y in top..=bottom {
            if !image.get_pixel(x, y).is_transparent() {
                return false;
            }
        }
        true
    };
    let mut left = 0;
    while is_column_transparent(left) {
        left += 1;
        assert!(left < image.width()); // otherwise we would have a transparent image
    }
    let mut right = image.width() - 1;
    while is_column_transparent(right) {
        right -= 1;
        assert!(right > left); // otherwise we would have a transparent image
    }
    let mut is_opaque = true;
    enum ColorState {
        Unset,
        Different,
        Rgb([u8; 3]),
    }
    let mut color = ColorState::Unset;
    'outer: for y in top..=bottom {
        for x in left..=right {
            let p = image.get_pixel(x, y);
            let alpha = p[3];
            if alpha != 255 {
                is_opaque = false;
            }
            if alpha == 0 {
                continue;
            }
            let get_pixel = || match source_format {
                SourceFormat::RgbaPremultiplied => <[u8; 3]>::try_from(&p.0[0..3])
                    .unwrap()
                    .map(|v| (v as u16 * 255 / alpha as u16) as u8),
                SourceFormat::Rgba => p.0[0..3].try_into().unwrap(),
            };
            match color {
                ColorState::Unset => {
                    color = ColorState::Rgb(get_pixel());
                }
                ColorState::Different => {
                    if !is_opaque {
                        break 'outer;
                    }
                }
                ColorState::Rgb([a, b, c]) => {
                    let px = get_pixel();
                    if a.abs_diff(px[0]) > 2 || b.abs_diff(px[1]) > 2 || c.abs_diff(px[2]) > 2 {
                        color = ColorState::Different
                    }
                }
            }
        }
    }

    let format = if let ColorState::Rgb(c) = color {
        PixelFormat::AlphaMap(c)
    } else if is_opaque {
        PixelFormat::Rgb
    } else {
        PixelFormat::RgbaPremultiplied
    };

    let rect = Rect::from_ltrb(left as _, top as _, (right + 1) as _, (bottom + 1) as _).unwrap();
    Texture {
        total_size: Size { width: image.width(), height: image.height() },
        original_size,
        rect,
        data: convert_image(image, source_format, format, rect),
        format,
    }
}

#[cfg(feature = "renderer-software")]
fn convert_image(
    image: image::RgbaImage,
    source_format: SourceFormat,
    format: PixelFormat,
    rect: Rect,
) -> Vec<u8> {
    let i = image::SubImage::new(&image, rect.x() as _, rect.y() as _, rect.width(), rect.height());
    match (source_format, format) {
        (_, PixelFormat::Rgb) => {
            i.pixels().flat_map(|(_, _, p)| IntoIterator::into_iter(p.0).take(3)).collect()
        }
        (SourceFormat::RgbaPremultiplied, PixelFormat::RgbaPremultiplied)
        | (SourceFormat::Rgba, PixelFormat::Rgba) => {
            i.pixels().flat_map(|(_, _, p)| IntoIterator::into_iter(p.0)).collect()
        }
        (SourceFormat::Rgba, PixelFormat::RgbaPremultiplied) => i
            .pixels()
            .flat_map(|(_, _, p)| {
                let a = p.0[3] as u32;
                IntoIterator::into_iter(p.0)
                    .take(3)
                    .map(move |x| (x as u32 * a / 255) as u8)
                    .chain(std::iter::once(a as u8))
            })
            .collect(),
        (SourceFormat::RgbaPremultiplied, PixelFormat::Rgba) => i
            .pixels()
            .flat_map(|(_, _, p)| {
                let a = p.0[3] as u32;
                IntoIterator::into_iter(p.0)
                    .take(3)
                    .map(move |x| (x as u32 * 255 / a) as u8)
                    .chain(std::iter::once(a as u8))
            })
            .collect(),
        (_, PixelFormat::AlphaMap(_)) => i.pixels().map(|(_, _, p)| p[3]).collect(),
    }
}

#[cfg(feature = "renderer-software")]
enum SourceFormat {
    RgbaPremultiplied,
    Rgba,
}

/// usvg renders SVG `<text>` against its own font database. The compiler has no
/// `SlintContext`, so resolve those fonts against the collection shared with
/// `embed_glyphs` (system fonts plus imported fonts) through the shared bridge.
#[cfg(feature = "renderer-software")]
fn svg_font_options(
    font_collection: Option<&SharedFontCollection>,
) -> resvg::usvg::Options<'static> {
    use i_slint_common::sharedfontique::svg as svg_fonts;

    let Some(font_collection) = font_collection.cloned() else {
        return resvg::usvg::Options::default();
    };
    svg_fonts::options(move |families, attributes, require_char| {
        let mut fonts = font_collection.lock().ok()?;
        let collection = &mut fonts.collection;
        svg_fonts::query_font(
            &mut collection.inner,
            &mut collection.source_cache,
            families,
            attributes,
            require_char,
        )
    })
}

#[cfg(feature = "renderer-software")]
fn load_image_from_bytes(
    data: &[u8],
    extension: Option<&str>,
    scale_factor: f32,
    font_collection: Option<&SharedFontCollection>,
) -> image::ImageResult<(image::RgbaImage, SourceFormat, Size)> {
    use resvg::{tiny_skia, usvg};

    let is_svg = matches!(extension, Some("svg") | Some("svgz"));

    if is_svg {
        let tree = {
            usvg::Tree::from_data(data, &svg_font_options(font_collection)).map_err(|e| {
                image::ImageError::Decoding(image::error::DecodingError::new(
                    image::error::ImageFormatHint::Name("svg".into()),
                    e,
                ))
            })?
        };

        let original_size = tree.size();
        let width = (original_size.width() * scale_factor) as u32;
        let height = (original_size.height() * scale_factor) as u32;

        let mut buffer = vec![0u8; width as usize * height as usize * 4];

        let size_error = || {
            image::ImageError::Limits(image::error::LimitError::from_kind(
                image::error::LimitErrorKind::DimensionError,
            ))
        };

        let mut skia_buffer =
            tiny_skia::PixmapMut::from_bytes(buffer.as_mut_slice(), width, height)
                .ok_or_else(size_error)?;

        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale_factor, scale_factor),
            &mut skia_buffer,
        );

        return image::RgbaImage::from_raw(width, height, buffer).ok_or_else(size_error).map(
            |img| {
                (
                    img,
                    SourceFormat::RgbaPremultiplied,
                    Size { width: original_size.width() as _, height: original_size.height() as _ },
                )
            },
        );
    }

    image::load_from_memory(data).map(|mut image| {
        let (original_width, original_height) = image.dimensions();

        if scale_factor < 1.0 {
            image = image.resize_exact(
                (original_width as f32 * scale_factor) as u32,
                (original_height as f32 * scale_factor) as u32,
                image::imageops::FilterType::Gaussian,
            );
        }

        (
            image.to_rgba8(),
            SourceFormat::Rgba,
            Size { width: original_width, height: original_height },
        )
    })
}

#[cfg(feature = "renderer-software")]
fn load_image(
    file: crate::fileaccess::VirtualFile,
    scale_factor: f32,
    font_collection: Option<&SharedFontCollection>,
    path: &str,
    diag: &mut BuildDiagnostics,
    source_location: &Option<crate::diagnostics::SourceLocation>,
) -> image::ImageResult<(image::RgbaImage, SourceFormat, Size)> {
    use std::ffi::OsStr;

    let extension = file.canon_path.extension().and_then(OsStr::to_str);

    let data = if let Some(buffer) = file.builtin_contents {
        buffer.to_vec()
    } else {
        std::fs::read(&file.canon_path)?
    };

    if source_is_animated(&data, extension) {
        diag.push_warning(
            format!(
                "{path} is an animated image, but compile-time texture embedding only \
                 keeps its first frame; the animation will not play"
            ),
            source_location,
        );
    }

    load_image_from_bytes(&data, extension, scale_factor, font_collection)
}

/// Cheap best-effort sniff for whether encoded image bytes represent a
/// multi-frame animation (GIF, animated PNG, animated WebP). Used only to warn
/// when compile-time texture embedding (`@image-url`, MCU/no_std targets with
/// no allocator for a frame array) is about to flatten one to a single static
/// frame. Not a decoder: returns `false` on anything malformed rather than
/// erroring, since getting this wrong just means a missed warning, not a
/// build failure.
#[cfg(feature = "renderer-software")]
fn source_is_animated(data: &[u8], extension: Option<&str>) -> bool {
    match extension.map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("gif") => gif_has_multiple_frames(data),
        Some("png") => png_is_apng(data),
        Some("webp") => webp_has_animation(data),
        _ => false,
    }
}

#[cfg(feature = "renderer-software")]
fn png_is_apng(data: &[u8]) -> bool {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !data.starts_with(SIG) {
        return false;
    }
    let mut pos = SIG.len();
    while pos + 8 <= data.len() {
        let Ok(len_bytes) = <[u8; 4]>::try_from(&data[pos..pos + 4]) else { return false };
        let len = u32::from_be_bytes(len_bytes) as usize;
        let chunk_type = &data[pos + 4..pos + 8];
        if chunk_type == b"acTL" {
            return true;
        }
        if chunk_type == b"IDAT" || chunk_type == b"IEND" {
            // acTL, if present, must appear before the first IDAT.
            return false;
        }
        // length(4) + type(4) + data(len) + crc(4)
        pos += 12usize.saturating_add(len);
    }
    false
}

#[cfg(feature = "renderer-software")]
fn webp_has_animation(data: &[u8]) -> bool {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return false;
    }
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let fourcc = &data[pos..pos + 4];
        let Ok(size_bytes) = <[u8; 4]>::try_from(&data[pos + 4..pos + 8]) else { return false };
        let size = u32::from_le_bytes(size_bytes) as usize;
        if fourcc == b"ANIM" {
            return true;
        }
        // Chunks are padded to an even size.
        pos += 8usize.saturating_add(size).saturating_add(size % 2);
    }
    false
}

/// Walks the GIF block structure just far enough to count image descriptors
/// (frames); a GIF with more than one is animated. Correctly skips extension
/// and image sub-block chains (each a length byte followed by that many
/// bytes, terminated by a zero-length block) so it isn't confused by pixel or
/// metadata bytes that happen to look like a block introducer.
#[cfg(feature = "renderer-software")]
fn gif_has_multiple_frames(data: &[u8]) -> bool {
    fn skip_sub_blocks(data: &[u8], pos: &mut usize) -> Option<()> {
        loop {
            let len = *data.get(*pos)? as usize;
            *pos += 1;
            if len == 0 {
                return Some(());
            }
            *pos += len;
        }
    }

    fn frame_count_at_least_two(data: &[u8]) -> Option<bool> {
        if !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
            return Some(false);
        }
        let mut pos = 6usize;
        // Logical screen descriptor: width(2) height(2) packed(1) bgcolor(1) pixel-aspect(1)
        let packed = *data.get(pos + 4)?;
        pos += 7;
        if packed & 0x80 != 0 {
            pos += 3usize << ((packed & 0x07) as usize + 1);
        }

        let mut frame_count = 0;
        loop {
            match *data.get(pos)? {
                0x21 => {
                    // Extension: introducer + label, then sub-blocks.
                    pos += 2;
                    skip_sub_blocks(data, &mut pos)?;
                }
                0x2C => {
                    frame_count += 1;
                    if frame_count >= 2 {
                        return Some(true);
                    }
                    // Image descriptor: left(2) top(2) width(2) height(2) packed(1)
                    let img_packed = *data.get(pos + 9)?;
                    pos += 10;
                    if img_packed & 0x80 != 0 {
                        pos += 3usize << ((img_packed & 0x07) as usize + 1);
                    }
                    // Image data: LZW min code size, then sub-blocks.
                    pos += 1;
                    skip_sub_blocks(data, &mut pos)?;
                }
                _ => return Some(false), // trailer (0x3B) or malformed data
            }
        }
    }

    frame_count_at_least_two(data).unwrap_or(false)
}

#[cfg(all(test, feature = "renderer-software"))]
mod animated_sniff_tests {
    use super::*;

    fn gif_with_frames(frame_count: usize) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"GIF89a");
        // Logical screen descriptor: 1x1, no global color table.
        data.extend_from_slice(&[1, 0, 1, 0, 0x00, 0, 0]);
        for _ in 0..frame_count {
            // Image descriptor: left=0 top=0 width=1 height=1, no local color table.
            data.push(0x2C);
            data.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0, 0x00]);
            // Image data: LZW min code size, one sub-block, then terminator.
            data.push(2);
            data.push(1);
            data.push(0x00);
            data.push(0);
        }
        data.push(0x3B);
        data
    }

    #[test]
    fn gif_single_frame_is_not_animated() {
        assert!(!gif_has_multiple_frames(&gif_with_frames(1)));
    }

    #[test]
    fn gif_multi_frame_is_animated() {
        assert!(gif_has_multiple_frames(&gif_with_frames(2)));
    }

    #[test]
    fn gif_with_extension_blocks_between_frames_is_still_detected() {
        let mut data = Vec::new();
        data.extend_from_slice(b"GIF89a");
        data.extend_from_slice(&[1, 0, 1, 0, 0x00, 0, 0]);
        // NETSCAPE2.0 application extension (loop count), a realistic real-world block.
        data.push(0x21);
        data.push(0xFF);
        data.push(11);
        data.extend_from_slice(b"NETSCAPE2.0");
        data.push(3);
        data.extend_from_slice(&[1, 0, 0]);
        data.push(0);
        data.extend_from_slice(&gif_with_frames(2)[13..]); // reuse the two-frame body
        assert!(gif_has_multiple_frames(&data));
    }

    #[test]
    fn non_gif_bytes_are_not_animated() {
        assert!(!gif_has_multiple_frames(b"not a gif"));
        assert!(!gif_has_multiple_frames(&[]));
    }

    fn png_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(chunk_type);
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&[0, 0, 0, 0]); // fake CRC; not validated by our sniff
        chunk
    }

    #[test]
    fn png_without_actl_is_not_apng() {
        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        data.extend(png_chunk(b"IHDR", &[0; 13]));
        data.extend(png_chunk(b"IDAT", &[]));
        data.extend(png_chunk(b"IEND", &[]));
        assert!(!png_is_apng(&data));
    }

    #[test]
    fn png_with_actl_before_idat_is_apng() {
        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        data.extend(png_chunk(b"IHDR", &[0; 13]));
        data.extend(png_chunk(b"acTL", &[0; 8]));
        data.extend(png_chunk(b"IDAT", &[]));
        data.extend(png_chunk(b"IEND", &[]));
        assert!(png_is_apng(&data));
    }

    #[test]
    fn non_png_bytes_are_not_apng() {
        assert!(!png_is_apng(b"not a png"));
    }

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(fourcc);
        chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunk.extend_from_slice(data);
        if data.len() % 2 != 0 {
            chunk.push(0);
        }
        chunk
    }

    fn riff_container(chunks: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        data.extend_from_slice(b"WEBP");
        data.extend_from_slice(chunks);
        data
    }

    #[test]
    fn static_webp_is_not_animated() {
        let chunks = riff_chunk(b"VP8 ", &[0; 4]);
        assert!(!webp_has_animation(&riff_container(&chunks)));
    }

    #[test]
    fn animated_webp_is_detected() {
        let mut chunks = riff_chunk(b"VP8X", &[0; 10]);
        chunks.extend(riff_chunk(b"ANIM", &[0; 6]));
        chunks.extend(riff_chunk(b"ANMF", &[0; 4]));
        assert!(webp_has_animation(&riff_container(&chunks)));
    }

    #[test]
    fn non_webp_bytes_are_not_animated() {
        assert!(!webp_has_animation(b"not a webp"));
    }
}

fn embed_data_uri(
    global_embedded_resources: &RefCell<TiVec<EmbeddedResourcesIdx, EmbeddedResources>>,
    path_to_id: &mut HashMap<SmolStr, EmbeddedResourcesIdx>,
    data_uri: &str,
    _embed_files: EmbedResourcesKind,
    _scale_factor: f32,
    diag: &mut BuildDiagnostics,
    source_location: &Option<crate::diagnostics::SourceLocation>,
    _font_collection: Option<&SharedFontCollection>,
) -> ImageReference {
    if let Some(&resource_id) = path_to_id.get(data_uri) {
        let resources = global_embedded_resources.borrow();
        return match &resources[resource_id].kind {
            #[cfg(feature = "renderer-software")]
            EmbeddedResourcesKind::TextureData { .. } => {
                ImageReference::EmbeddedTexture { resource_id }
            }
            EmbeddedResourcesKind::DataUriPayload(_, ext) => {
                ImageReference::EmbeddedData { resource_id, extension: ext.clone() }
            }
            _ => ImageReference::None,
        };
    }

    let (decoded_data, extension) = match crate::data_uri::decode_data_uri(data_uri) {
        Ok(result) => result,
        Err(e) => {
            diag.push_error(e, source_location);
            return ImageReference::None;
        }
    };

    let mut resources = global_embedded_resources.borrow_mut();
    let mut push = |kind| {
        let id = resources.push_and_get_key(EmbeddedResources { path: None, kind });
        path_to_id.insert(data_uri.into(), id);
        id
    };

    #[cfg(feature = "renderer-software")]
    if _embed_files == EmbedResourcesKind::EmbedTextures {
        if source_is_animated(&decoded_data, Some(&extension)) {
            diag.push_warning(
                "this data: URI is an animated image, but compile-time texture embedding \
                 only keeps its first frame; the animation will not play"
                    .into(),
                source_location,
            );
        }
        match load_image_from_bytes(
            &decoded_data,
            Some(&extension),
            _scale_factor,
            _font_collection,
        )
        .map_err(|e| e.to_string())
        {
            Ok((img, source_format, original_size)) => {
                let resource_id = push(EmbeddedResourcesKind::TextureData(generate_texture(
                    img,
                    source_format,
                    original_size,
                )));
                return ImageReference::EmbeddedTexture { resource_id };
            }
            Err(err) => {
                diag.push_error(format!("Cannot load data URI image: {err}"), source_location);
                return ImageReference::None;
            }
        }
    }

    let resource_id = push(EmbeddedResourcesKind::DataUriPayload(decoded_data, extension.clone()));

    ImageReference::EmbeddedData { resource_id, extension }
}
