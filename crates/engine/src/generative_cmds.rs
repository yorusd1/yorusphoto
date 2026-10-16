//! Capture a generation reference and place the returned image without changing source pixels.
//!
//! Generation runs outside the engine. The captured target carries the original document,
//! rectangle and selection coverage so a delayed result cannot follow a changed selection or tab.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use photocraft_algo::resample::{Resample, resize_surface_in_canvas};
use photocraft_cms::{Builtin, ColorSpace, Intent, Profile, Transform};
use photocraft_codecs::{ChannelLayout, DecodeOptions, EncodeOptions, Format, Image, Limits, SampleType as CodecSample};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const CAPTURE: &str = "ai.capture";
pub const APPLY: &str = "ai.apply";
pub const OPEN: &str = "ai.open";

/// Bounds keep both encoder work and decoded allocations independent of hostile dimensions.
pub const MAX_PIXELS: u64 = 16_777_216;
pub const MAX_SIDE: u32 = 16_384;
const MAX_ENCODED_BYTES: usize = 64 << 20;
const MAX_BASE64_CHARS: usize = MAX_ENCODED_BYTES.div_ceil(3) * 4;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    document: u64,
    canvas: [u32; 2],
    anchor_layer: Option<u64>,
    rect: [i32; 4],
    coverage: Option<String>,
}

fn bad(command: &str, message: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: command.into(), msg: message.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn checked_size(command: &str, width: u32, height: u32) -> Result<usize> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || pixels > MAX_PIXELS {
        return Err(bad(command, format!("image area must be nonempty, at most {MAX_SIDE} pixels per side and {MAX_PIXELS} pixels in total")));
    }
    usize::try_from(pixels).map_err(|_| bad(command, "image area does not fit in memory"))
}

fn encoded_png(image: &Image, command: &str) -> Result<String> {
    let options = EncodeOptions { embed_metadata: false, ..Default::default() };
    let bytes = photocraft_codecs::encode(image, Format::Png, &options).map_err(|e| bad(command, format!("cannot encode image: {e}")))?;
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(bad(command, "encoded image exceeds 64 MiB"));
    }
    Ok(STANDARD.encode(bytes))
}

fn decoded_image(data: &str, command: &str, coverage: bool) -> Result<Image> {
    if data.len() > MAX_BASE64_CHARS {
        return Err(bad(command, "image data exceeds 64 MiB"));
    }
    let bytes = STANDARD.decode(data).map_err(|_| bad(command, "image data must be valid base64"))?;
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(bad(command, "image data exceeds 64 MiB"));
    }
    let format = photocraft_codecs::detect(&bytes).ok_or_else(|| bad(command, "image data is not PNG, JPEG or WebP"))?;
    if (coverage && format != Format::Png) || (!coverage && !matches!(format, Format::Png | Format::Jpeg | Format::WebP)) {
        return Err(bad(command, if coverage { "selection coverage must be a grayscale PNG" } else { "returned image must be PNG, JPEG or WebP" }));
    }
    let opts =
        DecodeOptions { limits: Limits { max_width: MAX_SIDE, max_height: MAX_SIDE, max_pixels: MAX_PIXELS, max_alloc: 128 << 20 }, ..Default::default() };
    let image = photocraft_codecs::decode_with(&bytes, &opts).map_err(|e| bad(command, format!("cannot decode image: {e}")))?;
    checked_size(command, image.width(), image.height())?;
    if coverage && image.layout() != ChannelLayout::Gray {
        return Err(bad(command, "selection coverage must be a grayscale PNG without alpha"));
    }
    Ok(image)
}

/// Read the visible merged rectangle as sRGB PNG; capture never changes history or clipboard.
pub fn capture(s: &mut Session, p: &Value) -> Result<Value> {
    if p.as_object().is_none_or(|p| !p.is_empty()) {
        return Err(bad(CAPTURE, "capture accepts an empty object"));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &st.doc;
    let canvas = doc.bounds();
    let coordinate_limit = i32::MAX as u32 - photocraft_geom::TILE_SIZE as u32;
    if doc.size.width > coordinate_limit || doc.size.height > coordinate_limit {
        return Err(bad(CAPTURE, "canvas dimensions exceed document coordinates"));
    }
    let area = doc.selection.as_ref().map_or(canvas, |mask| mask.content_bounds().intersect(&canvas));
    let count = checked_size(CAPTURE, area.width(), area.height())?;
    crate::allocation::checkpoint("capturing the generation reference")?;
    let buffer = photocraft_compose::render(doc, area);
    let mut rgba = crate::allocation::capacity(count * 4, "capturing the generation reference")?;
    rgba.extend(buffer.px.iter().flatten().copied());
    let profile = crate::color_cmds::composite_profile(doc);
    let transform = Transform::new(&profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .map_err(|e| bad(CAPTURE, format!("cannot convert reference to sRGB: {e}")))?;
    transform.apply(&mut rgba, 4);
    let image = Image::from_normalized(area.width(), area.height(), ChannelLayout::Rgba, CodecSample::U8, &rgba)
        .map_err(|e| bad(CAPTURE, e.to_string()))?
        .with_icc(Some(Builtin::Srgb.profile().to_bytes().to_vec()));
    let reference = encoded_png(&image, CAPTURE)?;
    let coverage = match &doc.selection {
        Some(mask) => {
            let values = crate::allocation::read_region(mask, area, "capturing the original selection")?;
            let channel_count = mask.format().channels().max(1);
            let mut gray = crate::allocation::capacity(count, "capturing the original selection")?;
            gray.extend(values.chunks_exact(channel_count).map(|pixel| pixel.first().copied().unwrap_or(0.0).clamp(0.0, 1.0)));
            let image =
                Image::from_normalized(area.width(), area.height(), ChannelLayout::Gray, CodecSample::U16, &gray).map_err(|e| bad(CAPTURE, e.to_string()))?;
            Some(encoded_png(&image, CAPTURE)?)
        }
        None => None,
    };
    let target = Target {
        document: doc.id.0,
        canvas: [doc.size.width, doc.size.height],
        anchor_layer: st.active_layer.map(|layer| layer.0),
        rect: [area.x0, area.y0, area.x1, area.y1],
        coverage,
    };
    Ok(json!({"target": target, "reference": reference, "mimeType": "image/png", "width": area.width(), "height": area.height()}))
}

fn target_from(p: &Value, s: &Session) -> Result<(Target, Rect)> {
    let value = p.get("target").ok_or_else(|| bad(APPLY, "missing captured `target`"))?;
    if value.get("coverage").and_then(Value::as_str).is_some_and(|data| data.len() > MAX_BASE64_CHARS) {
        return Err(bad(APPLY, "captured selection data exceeds 64 MiB"));
    }
    let target = Target::deserialize(value).map_err(|e| bad(APPLY, format!("invalid captured target: {e}")))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let coordinate_limit = i32::MAX as u32 - photocraft_geom::TILE_SIZE as u32;
    if st.doc.size.width > coordinate_limit || st.doc.size.height > coordinate_limit {
        return Err(bad(APPLY, "canvas dimensions exceed safe document coordinates"));
    }
    if target.document != st.doc.id.0 {
        return Err(bad(APPLY, "the source document is not active; switch back to the document captured for generation"));
    }
    if target.canvas != [st.doc.size.width, st.doc.size.height] {
        return Err(bad(APPLY, "the source canvas size changed after generation started"));
    }
    if target.anchor_layer.is_some_and(|id| st.doc.layer(LayerId(id)).is_none()) {
        return Err(bad(APPLY, "the original anchor layer was removed after generation started"));
    }
    let [x0, y0, x1, y1] = target.rect;
    let area = Rect::new(x0, y0, x1, y1);
    checked_size(APPLY, area.width(), area.height())?;
    if x0 < 0 || y0 < 0 || !st.doc.bounds().contains_rect(&area) {
        return Err(bad(APPLY, "captured rectangle must lie inside the original canvas"));
    }
    Ok((target, area))
}

/// The decoded reference's RGB profile, including grayscale profiles expanded to RGB.
fn image_profile(image: &Image, command: &str) -> Result<Profile> {
    if let Some(bytes) = image.icc.as_ref() {
        let profile = Profile::parse(bytes).map_err(|e| bad(command, format!("returned image has an invalid color profile: {e}")))?;
        return match (image.layout(), profile.color_space) {
            (ChannelLayout::Rgb | ChannelLayout::Rgba, ColorSpace::Rgb) => Ok(profile),
            (ChannelLayout::Gray | ChannelLayout::GrayA, ColorSpace::Gray) => {
                profile.gray_as_rgb().ok_or_else(|| bad(command, "returned grayscale color profile cannot be converted to RGB"))
            }
            _ => Err(bad(command, "returned image color profile does not match its pixels")),
        };
    }
    Ok(Builtin::Srgb.profile().clone())
}

/// Insert one color-managed pixel layer at the original size and original selection coverage.
pub fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let (target, area) = target_from(p, s)?;
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(APPLY, "missing base64 image `data`"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => "Generated image",
        Some(value) => value.as_str().filter(|name| !name.trim().is_empty() && name.len() <= 256).ok_or_else(|| bad(APPLY, "name must be 1 to 256 bytes"))?,
    };
    crate::allocation::checkpoint("placing the generated image")?;
    let coverage = match target.coverage.as_deref() {
        None => None,
        Some(data) => {
            let image = decoded_image(data, APPLY, true)?;
            if image.dimensions() != (area.width(), area.height()) {
                return Err(bad(APPLY, "captured selection coverage does not match the original rectangle"));
            }
            let values = image.to_normalized();
            if !values.iter().any(|value| *value > 0.0) {
                return Err(bad(APPLY, "captured selection is empty"));
            }
            Some(values)
        }
    };
    let image = decoded_image(data, APPLY, false)?;
    if !image.layout().is_rgb() && !image.layout().is_gray() {
        return Err(bad(APPLY, "returned image must use RGB or grayscale pixels"));
    }
    let profile = image_profile(&image, APPLY)?;
    let source_area = Rect::from_xywh(0, 0, image.width(), image.height());
    let rgba = image.to_rgba_f32();
    let mut source = Surface::new(PixelFormat::RGBA32F);
    source.try_write_region(source_area, &rgba).map_err(|e| bad(APPLY, format!("cannot allocate returned image: {e}")))?;
    drop(rgba);
    // Repeat border pixels during scaling, as Image Size does, so an opaque result stays opaque
    // to the selected rectangle's edge. The final write keeps the rectangle exact even with
    // fully transparent edges in the generated PNG.
    let resized = resize_surface_in_canvas(
        &source,
        f64::from(area.width()) / f64::from(image.width()),
        f64::from(area.height()) / f64::from(image.height()),
        Resample::Bicubic,
        source_area,
    );
    let local_area = Rect::from_xywh(0, 0, area.width(), area.height());
    let rgba = crate::allocation::read_region(&resized, local_area, "resampling the generated image")?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let format = st.doc.pixel_format();
    let destination_profile = crate::color_cmds::document_profile(&st.doc);
    let transform = Transform::new(&profile, &destination_profile, Intent::RelativeColorimetric, true)
        .map_err(|e| bad(APPLY, format!("cannot convert image to document color: {e}")))?;
    let count = checked_size(APPLY, area.width(), area.height())?;
    let channels = format.channels();
    let mut pixels = crate::allocation::filled(count * channels, 0.0f32, "converting the generated image")?;
    transform.convert_f32(&rgba, 4, &mut pixels, channels, true);
    if let Some(coverage) = coverage {
        for (pixel, weight) in pixels.chunks_exact_mut(channels).zip(coverage) {
            if let Some(alpha) = pixel.last_mut() {
                *alpha *= weight.clamp(0.0, 1.0);
            }
        }
    }
    let mut surface = Surface::new(format);
    surface.try_write_region(area, &pixels).map_err(|e| bad(APPLY, format!("cannot allocate generated layer: {e}")))?;
    surface.prune();
    let id = s.edit("Generate Image", |doc, active| {
        let layer = Layer::new(name, LayerContent::Raster(surface));
        let id = doc.insert_above(target.anchor_layer.map(LayerId), layer);
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({"layer": id.0, "rect": target.rect, "bounds": [area.x0, area.y0, area.width(), area.height()]}))
}

/// Open an independently generated image on a fresh sRGB canvas, optionally at an exact size.
pub fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(OPEN, "missing base64 image `data`"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => "Generated image",
        Some(value) => value.as_str().filter(|name| !name.trim().is_empty() && name.len() <= 256).ok_or_else(|| bad(OPEN, "name must be 1 to 256 bytes"))?,
    };
    let dimension = |field| match p.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().and_then(|v| u32::try_from(v).ok()).map(Some).ok_or_else(|| bad(OPEN, format!("`{field}` must be a positive integer"))),
    };
    let dimensions = match (dimension("width")?, dimension("height")?) {
        (None, None) => None,
        (Some(width), Some(height)) => {
            checked_size(OPEN, width, height)?;
            Some((width, height))
        }
        _ => return Err(bad(OPEN, "provide both `width` and `height`, or omit both")),
    };
    crate::allocation::checkpoint("opening the generated image")?;
    let image = decoded_image(data, OPEN, false)?;
    if !image.layout().is_rgb() && !image.layout().is_gray() {
        return Err(bad(OPEN, "returned image must use RGB or grayscale pixels"));
    }
    let (width, height) = dimensions.unwrap_or_else(|| image.dimensions());
    let profile = image_profile(&image, OPEN)?;
    let transform = Transform::new(&profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .map_err(|e| bad(OPEN, format!("cannot convert returned image to sRGB: {e}")))?;
    let mut rgba = image.to_rgba_f32();
    transform.apply(&mut rgba, 4);
    let source_area = Rect::from_xywh(0, 0, image.width(), image.height());
    let mut source = Surface::new(PixelFormat::RGBA32F);
    source.try_write_region(source_area, &rgba).map_err(|e| bad(OPEN, format!("cannot allocate returned image: {e}")))?;
    drop(rgba);
    let resized = resize_surface_in_canvas(
        &source,
        f64::from(width) / f64::from(image.width()),
        f64::from(height) / f64::from(image.height()),
        Resample::Bicubic,
        source_area,
    );
    let area = Rect::from_xywh(0, 0, width, height);
    let pixels = crate::allocation::read_region(&resized, area, "resampling the generated image")?;
    let depth = if image.sample_type() == CodecSample::U16 { photocraft_color::SampleType::U16 } else { photocraft_color::SampleType::U8 };
    let mut document = Document::new(name, Size::new(width, height), photocraft_color::ColorMode::Rgb, depth);
    document.icc_profile = Some(std::sync::Arc::new(Builtin::Srgb.profile().to_bytes().to_vec()));
    let mut surface = Surface::new(document.pixel_format());
    surface.try_write_region(area, &pixels).map_err(|e| bad(OPEN, format!("cannot allocate generated canvas: {e}")))?;
    surface.prune();
    let layer = Layer::new("Generated image", LayerContent::Raster(surface));
    let id = layer.id;
    document.layers.push(layer);
    let index = s.add_document(document, None);
    Ok(json!({"document": index, "layer": id.0, "width": width, "height": height}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CAPTURE,
            label: "Capture Generation Reference",
            menu: &[],
            shortcut: None,
            params: "{} (capture visible merged selection bounds, or canvas, as sRGB PNG; returns reference, target, width, height; no document edit)",
            enabled: has_doc,
            run: capture,
            journal: false,
        },
        CommandSpec {
            id: APPLY,
            label: "Place Generated Image",
            menu: &[],
            shortcut: None,
            params: r#"{"target":json (the unmodified ai.capture target),"data":text (base64 PNG/JPEG/WebP),"name":text?} (one new pixel layer at captured bounds and coverage; rejects a different active document or changed canvas)"#,
            enabled: has_doc,
            run: apply,
            journal: true,
        },
        CommandSpec {
            id: OPEN,
            label: "Open Generated Image",
            menu: &[],
            shortcut: None,
            params: r#"{"data":text (base64 PNG/JPEG/WebP),"name":text?,"width":u32?,"height":u32?} (new independent sRGB document; both dimensions optional, provide both for exact canvas size)"#,
            enabled: crate::commands::always,
            run: open,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "generative_cmds_tests.rs"]
mod tests;
