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

/// Most frames one generated animation may place: a flipbook, not a video.
pub const MAX_FRAMES: usize = 256;

/// The returned picture as frames with their on-screen times in milliseconds: a still PNG, JPEG
/// or WebP is one frame (time 0); an animated GIF or WebP is every frame, each composited onto
/// the animation's full canvas by the decoder. All frames share one size.
fn decoded_frames(data: &str, command: &str) -> Result<Vec<(Image, u32)>> {
    if data.len() > MAX_BASE64_CHARS {
        return Err(bad(command, "image data exceeds 64 MiB"));
    }
    let bytes = STANDARD.decode(data).map_err(|_| bad(command, "image data must be valid base64"))?;
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(bad(command, "image data exceeds 64 MiB"));
    }
    let format = photocraft_codecs::detect(&bytes).ok_or_else(|| bad(command, "image data is not PNG, JPEG, WebP or GIF"))?;
    // Every frame is held decoded at once: the byte budget covers the whole animation.
    let opts =
        DecodeOptions { limits: Limits { max_width: MAX_SIDE, max_height: MAX_SIDE, max_pixels: MAX_PIXELS, max_alloc: 512 << 20 }, ..Default::default() };
    let animation = |frames: std::result::Result<Vec<photocraft_codecs::AnimationFrame>, photocraft_codecs::CodecError>| {
        frames.map(|frames| frames.into_iter().map(|f| (f.image, f.delay_ms)).collect::<Vec<_>>()).map_err(|e| bad(command, format!("cannot decode animation: {e}")))
    };
    let frames = match format {
        Format::Gif => animation(photocraft_codecs::decode_gif_frames(&bytes, &opts))?,
        Format::WebP => animation(photocraft_codecs::decode_webp_frames(&bytes, &opts))?,
        Format::Png | Format::Jpeg => {
            vec![(photocraft_codecs::decode_with(&bytes, &opts).map_err(|e| bad(command, format!("cannot decode image: {e}")))?, 0)]
        }
        _ => return Err(bad(command, "returned image must be PNG, JPEG, WebP or GIF")),
    };
    let Some(size) = frames.first().map(|(image, _)| image.dimensions()) else {
        return Err(bad(command, "the returned animation has no frames"));
    };
    if frames.len() > MAX_FRAMES {
        return Err(bad(command, format!("the returned animation has more than {MAX_FRAMES} frames")));
    }
    for (image, _) in &frames {
        checked_size(command, image.width(), image.height())?;
        if image.dimensions() != size {
            return Err(bad(command, "the animation's frames differ in size"));
        }
        if !image.layout().is_rgb() && !image.layout().is_gray() {
            return Err(bad(command, "returned image must use RGB or grayscale pixels"));
        }
    }
    Ok(frames)
}

/// `grid`: a still image laid out as a flipbook sheet, `{columns, rows, count?}`, is cut into its
/// first `count` cells in reading order (left to right, then top to bottom; the remainder of an
/// uneven division is left out). An animation, or a still with no `grid`, is left as it is. This is
/// how a sheet drawn in place of a GIF (an image model without GIF output) becomes its frames.
fn grid_frames(p: &Value, frames: Vec<(Image, u32)>, command: &str) -> Result<Vec<(Image, u32)>> {
    let Some(grid) = p.get("grid").filter(|grid| !grid.is_null()) else { return Ok(frames) };
    let number = |key: &str| grid.get(key).and_then(Value::as_u64).filter(|n| (1..=16).contains(n)).and_then(|n| u32::try_from(n).ok());
    let (Some(columns), Some(rows)) = (number("columns"), number("rows")) else {
        return Err(bad(command, "`grid` needs whole `columns` and `rows` from 1 to 16"));
    };
    let cells = columns * rows;
    let count = match grid.get("count") {
        None | Some(Value::Null) => cells,
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=u64::from(cells)).contains(n))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad(command, "`grid.count` must be from 1 to columns × rows"))?,
    };
    if frames.len() != 1 || count < 2 {
        return Ok(frames);
    }
    let Some((image, _)) = frames.into_iter().next() else { return Err(bad(command, "the returned image has no frames")) };
    let (width, height) = (image.width() / columns, image.height() / rows);
    if width == 0 || height == 0 {
        return Err(bad(command, "the flipbook grid has more cells than the image has pixels"));
    }
    let pixel = image.layout().channels() * image.sample_type().bytes();
    let (row_bytes, cell_bytes) = (image.width() as usize * pixel, width as usize * pixel);
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count {
        let (x, y) = ((index % columns) * width, (index / columns) * height);
        let mut data = Vec::with_capacity(cell_bytes * height as usize);
        for line in y..y + height {
            let start = line as usize * row_bytes + x as usize * pixel;
            data.extend_from_slice(image.data().get(start..start + cell_bytes).ok_or_else(|| bad(command, "the flipbook grid falls outside the image"))?);
        }
        let mut cell = Image::from_raw(width, height, image.layout(), image.sample_type(), data).map_err(|e| bad(command, format!("cannot cut the flipbook grid: {e}")))?;
        cell.icc.clone_from(&image.icc);
        cell.cicp.clone_from(&image.cicp);
        out.push((cell, 0));
    }
    Ok(out)
}

/// A frame's layer name: the requested name for a still, numbered with its time for animations.
fn frame_name(name: &str, index: usize, count: usize, delay_ms: u32) -> String {
    if count <= 1 {
        return name.to_owned();
    }
    let number = index.saturating_add(1);
    if delay_ms > 0 { format!("{name} · frame {number} of {count} · {delay_ms} ms") } else { format!("{name} · frame {number} of {count}") }
}

/// One frame as a layer's pixels at `area`: resampled to the rectangle (repeating border pixels,
/// as Image Size does), converted to the document's colour and masked by the captured coverage.
fn frame_surface(image: &Image, area: Rect, format: PixelFormat, destination: &Profile, coverage: Option<&[f32]>, command: &str) -> Result<Surface> {
    let profile = image_profile(image, command)?;
    let source_area = Rect::from_xywh(0, 0, image.width(), image.height());
    let rgba = image.to_rgba_f32();
    let mut source = Surface::new(PixelFormat::RGBA32F);
    source.try_write_region(source_area, &rgba).map_err(|e| bad(command, format!("cannot allocate returned image: {e}")))?;
    drop(rgba);
    // The final write keeps the rectangle exact even with fully transparent edges in the PNG.
    let resized = resize_surface_in_canvas(
        &source,
        f64::from(area.width()) / f64::from(image.width()),
        f64::from(area.height()) / f64::from(image.height()),
        Resample::Bicubic,
        source_area,
    );
    drop(source);
    let local_area = Rect::from_xywh(0, 0, area.width(), area.height());
    let rgba = crate::allocation::read_region(&resized, local_area, "resampling the generated image")?;
    drop(resized);
    let transform = Transform::new(&profile, destination, Intent::RelativeColorimetric, true)
        .map_err(|e| bad(command, format!("cannot convert image to document color: {e}")))?;
    let count = checked_size(command, area.width(), area.height())?;
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
    surface.try_write_region(area, &pixels).map_err(|e| bad(command, format!("cannot allocate generated layer: {e}")))?;
    surface.prune();
    Ok(surface)
}

/// The largest document a growing placement (`"fit":"grow"`) makes: room for big game textures
/// and sprite sheets, bounded so one result can never ask for an unbounded canvas.
pub const MAX_GROWN_SIDE: u32 = 16_384;
pub const MAX_GROWN_PIXELS: u64 = 100_000_000;

/// How `ai.apply` treats a result with more pixels than the captured rectangle: `"exact"` (the
/// default) resamples it into the rectangle; `"grow"` first enlarges the whole document so the
/// rectangle holds the generated pixels, keeping the layer's place and size in the picture.
fn wants_growth(p: &Value) -> Result<bool> {
    match p.get("fit") {
        None | Some(Value::Null) => Ok(false),
        Some(value) => match value.as_str() {
            Some("exact") => Ok(false),
            Some("grow") => Ok(true),
            _ => Err(bad(APPLY, "`fit` must be \"exact\" or \"grow\"")),
        },
    }
}

/// The scale (1 or more) at which `area` of a `canvas` holds `image` pixel for pixel, within
/// [`MAX_GROWN_SIDE`] and [`MAX_GROWN_PIXELS`]; 1 when nothing needs to grow.
fn growth_scale(canvas: [u32; 2], area: Rect, image: (u32, u32)) -> f64 {
    let [width, height] = canvas;
    if width == 0 || height == 0 || area.width() == 0 || area.height() == 0 {
        return 1.0;
    }
    let need = (f64::from(image.0) / f64::from(area.width())).max(f64::from(image.1) / f64::from(area.height()));
    let by_side = f64::from(MAX_GROWN_SIDE) / f64::from(width.max(height));
    let by_pixels = (MAX_GROWN_PIXELS as f64 / (f64::from(width) * f64::from(height))).sqrt();
    let scale = need.min(by_side).min(by_pixels);
    if scale.is_finite() && scale > 1.0 + 1e-6 { scale } else { 1.0 }
}

/// Bilinear resampling of selection coverage (0..1), used when a placement enlarges the document.
fn resample_coverage(values: &[f32], from: (u32, u32), to: (u32, u32)) -> Vec<f32> {
    let (fw, fh) = (from.0.max(1) as usize, from.1.max(1) as usize);
    let (tw, th) = (to.0 as usize, to.1 as usize);
    let at = |x: usize, y: usize| values.get(y.min(fh - 1) * fw + x.min(fw - 1)).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(tw.saturating_mul(th));
    for y in 0..th {
        let sy = ((y as f64 + 0.5) * fh as f64 / th.max(1) as f64 - 0.5).max(0.0);
        let (y0, fy) = (sy.floor() as usize, (sy - sy.floor()) as f32);
        for x in 0..tw {
            let sx = ((x as f64 + 0.5) * fw as f64 / tw.max(1) as f64 - 0.5).max(0.0);
            let (x0, fx) = (sx.floor() as usize, (sx - sx.floor()) as f32);
            let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
            let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
            out.push((top * (1.0 - fy) + bottom * fy).clamp(0.0, 1.0));
        }
    }
    out
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

/// Insert the returned picture as color-managed pixel layers at the captured rectangle and
/// selection coverage: one layer for a still image, one per frame for an animation (the first
/// shown, the rest hidden, bottom to top, like a flipbook's pages). With `"fit":"grow"` a picture
/// with more pixels than the rectangle first enlarges the document ([`wants_growth`]).
pub fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let (mut target, mut area) = target_from(p, s)?;
    let grow = wants_growth(p)?;
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(APPLY, "missing base64 image `data`"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => "Generated image",
        Some(value) => value.as_str().filter(|name| !name.trim().is_empty() && name.len() <= 200).ok_or_else(|| bad(APPLY, "name must be 1 to 200 bytes"))?,
    };
    crate::allocation::checkpoint("placing the generated image")?;
    let mut coverage = match target.coverage.as_deref() {
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
    let frames = grid_frames(p, decoded_frames(data, APPLY)?, APPLY)?;
    for (image, _) in &frames {
        image_profile(image, APPLY)?;
    }
    let picture = frames.first().map(|(image, _)| image.dimensions()).ok_or_else(|| bad(APPLY, "the returned animation has no frames"))?;
    let mut grown = None;
    let scale = if grow { growth_scale(target.canvas, area, picture) } else { 1.0 };
    if scale > 1.0 {
        let [width, height] = target.canvas;
        let wanted = |side: u32| (f64::from(side) * scale).round().clamp(1.0, f64::from(MAX_GROWN_SIDE)) as u32;
        crate::allocation::checkpoint("enlarging the document for the generated image")?;
        s.execute("image.imageSize", json!({"width": wanted(width), "height": wanted(height), "resample": "bicubicSmoother"}))?;
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let (new_width, new_height) = (st.doc.size.width, st.doc.size.height);
        let (sx, sy) = (f64::from(new_width) / f64::from(width), f64::from(new_height) / f64::from(height));
        let edge = |value: i32, factor: f64, limit: u32| ((f64::from(value) * factor).round() as i64).clamp(0, i64::from(limit)) as i32;
        let scaled = Rect::new(edge(area.x0, sx, new_width), edge(area.y0, sy, new_height), edge(area.x1, sx, new_width), edge(area.y1, sy, new_height));
        checked_size(APPLY, scaled.width(), scaled.height())?;
        if let Some(values) = coverage.take() {
            let resampled = resample_coverage(&values, (area.width(), area.height()), (scaled.width(), scaled.height()));
            let image = Image::from_normalized(scaled.width(), scaled.height(), ChannelLayout::Gray, CodecSample::U16, &resampled)
                .map_err(|e| bad(APPLY, e.to_string()))?;
            target.coverage = Some(encoded_png(&image, APPLY)?);
            coverage = Some(resampled);
        }
        target.canvas = [new_width, new_height];
        target.rect = [scaled.x0, scaled.y0, scaled.x1, scaled.y1];
        area = scaled;
        grown = Some(json!({"width": new_width, "height": new_height, "scale": sx}));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let format = st.doc.pixel_format();
    let destination = crate::color_cmds::document_profile(&st.doc);
    let count = frames.len();
    let mut layers = Vec::with_capacity(count);
    for (index, (image, delay)) in frames.iter().enumerate() {
        crate::allocation::checkpoint("placing a generated frame")?;
        layers.push((frame_name(name, index, count, *delay), frame_surface(image, area, format, &destination, coverage.as_deref(), APPLY)?));
    }
    let delays: Vec<u32> = frames.iter().map(|(_, delay)| *delay).collect();
    drop(frames);
    let ids = s.edit(if count > 1 { "Generate Animation" } else { "Generate Image" }, |doc, active| {
        let mut below = target.anchor_layer.map(LayerId);
        let mut ids = Vec::with_capacity(layers.len());
        for (index, (layer_name, surface)) in layers.into_iter().enumerate() {
            let mut layer = Layer::new(layer_name, LayerContent::Raster(surface));
            // A flipbook shows its first page; the others wait hidden above it.
            layer.visible = index == 0;
            let id = doc.insert_above(below, layer);
            below = Some(id);
            ids.push(id);
        }
        *active = ids.first().copied();
        Ok(ids)
    })?;
    let first = ids.first().map(|id| id.0);
    let mut out = json!({
        "layer": first,
        "layers": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
        "frames": count,
        "delays": delays,
        "rect": target.rect,
        "bounds": [area.x0, area.y0, area.width(), area.height()],
    });
    // A grown document has a new size and rectangle: later results of the same request place with
    // this target instead of the one captured before the growth.
    if let (Some(grown), Some(fields)) = (grown, out.as_object_mut()) {
        fields.insert("grown".into(), grown);
        fields.insert("target".into(), serde_json::to_value(&target).map_err(|e| bad(APPLY, e.to_string()))?);
    }
    Ok(out)
}

/// One frame converted to sRGB pixels at `size`, for a new document.
fn open_pixels(image: &Image, size: (u32, u32), command: &str) -> Result<Vec<f32>> {
    let (width, height) = size;
    let profile = image_profile(image, command)?;
    let transform = Transform::new(&profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .map_err(|e| bad(command, format!("cannot convert returned image to sRGB: {e}")))?;
    let mut rgba = image.to_rgba_f32();
    transform.apply(&mut rgba, 4);
    let source_area = Rect::from_xywh(0, 0, image.width(), image.height());
    let mut source = Surface::new(PixelFormat::RGBA32F);
    source.try_write_region(source_area, &rgba).map_err(|e| bad(command, format!("cannot allocate returned image: {e}")))?;
    drop(rgba);
    let resized = resize_surface_in_canvas(
        &source,
        f64::from(width) / f64::from(image.width()),
        f64::from(height) / f64::from(image.height()),
        Resample::Bicubic,
        source_area,
    );
    crate::allocation::read_region(&resized, Rect::from_xywh(0, 0, width, height), "resampling the generated image")
}

/// The columns of a flipbook sheet for `count` frames: the given number, or the smallest square
/// grid that holds them (2x2, 4x4, 8x8 for 4, 16, 64 frames, as game engines' flipbooks use).
fn sheet_columns(p: &Value, count: usize) -> Result<usize> {
    match p.get("columns") {
        None | Some(Value::Null) => {
            let mut side = 1usize;
            while side.saturating_mul(side) < count {
                side += 1;
            }
            Ok(side)
        }
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MAX_FRAMES).contains(n))
            .ok_or_else(|| bad(OPEN, format!("`columns` must be a whole number from 1 to {MAX_FRAMES}"))),
    }
}

/// Open the returned picture on a fresh sRGB canvas, optionally with each frame at an exact size.
/// An animation opens as one layer per frame (`"frames":"layers"`, the first shown) or as a
/// flipbook sheet, its frames in a grid on one layer (`"frames":"sheet"`, `"columns"`).
pub fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(OPEN, "missing base64 image `data`"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => "Generated image",
        Some(value) => value.as_str().filter(|name| !name.trim().is_empty() && name.len() <= 200).ok_or_else(|| bad(OPEN, "name must be 1 to 200 bytes"))?,
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
    let sheet = match p.get("frames").and_then(Value::as_str) {
        None | Some("layers") => false,
        Some("sheet") => true,
        Some(_) => return Err(bad(OPEN, "`frames` must be \"layers\" or \"sheet\"")),
    };
    crate::allocation::checkpoint("opening the generated image")?;
    let frames = grid_frames(p, decoded_frames(data, OPEN)?, OPEN)?;
    let first = frames.first().map(|(image, _)| image).ok_or_else(|| bad(OPEN, "the returned animation has no frames"))?;
    let (width, height) = dimensions.unwrap_or_else(|| first.dimensions());
    let depth = if first.sample_type() == CodecSample::U16 { photocraft_color::SampleType::U16 } else { photocraft_color::SampleType::U8 };
    let count = frames.len();
    let columns = if sheet { sheet_columns(p, count)?.min(count.max(1)) } else { 1 };
    let rows = if sheet { count.div_ceil(columns) } else { 1 };
    let to_u32 = |n: usize| u32::try_from(n).map_err(|_| bad(OPEN, "the flipbook sheet is too large"));
    let (canvas_width, canvas_height) = if sheet {
        (width.checked_mul(to_u32(columns)?).ok_or_else(|| bad(OPEN, "the flipbook sheet is too large"))?, height.checked_mul(to_u32(rows)?).ok_or_else(|| bad(OPEN, "the flipbook sheet is too large"))?)
    } else {
        (width, height)
    };
    checked_size(OPEN, canvas_width, canvas_height)?;
    let mut document = Document::new(name, Size::new(canvas_width, canvas_height), photocraft_color::ColorMode::Rgb, depth);
    document.icc_profile = Some(std::sync::Arc::new(Builtin::Srgb.profile().to_bytes().to_vec()));
    let mut ids = Vec::with_capacity(count);
    if sheet {
        let mut surface = Surface::new(document.pixel_format());
        for (index, (image, _)) in frames.iter().enumerate() {
            crate::allocation::checkpoint("laying out the flipbook sheet")?;
            let pixels = open_pixels(image, (width, height), OPEN)?;
            let (column, row) = (to_u32(index % columns)?, to_u32(index / columns)?);
            let cell = Rect::from_xywh(i32::try_from(column * width).map_err(|_| bad(OPEN, "the flipbook sheet is too large"))?, i32::try_from(row * height).map_err(|_| bad(OPEN, "the flipbook sheet is too large"))?, width, height);
            surface.try_write_region(cell, &pixels).map_err(|e| bad(OPEN, format!("cannot allocate the flipbook sheet: {e}")))?;
        }
        surface.prune();
        let layer = Layer::new(format!("{name} · flipbook {columns}x{rows}"), LayerContent::Raster(surface));
        ids.push(layer.id);
        document.layers.push(layer);
    } else {
        for (index, (image, delay)) in frames.iter().enumerate() {
            crate::allocation::checkpoint("opening a generated frame")?;
            let pixels = open_pixels(image, (width, height), OPEN)?;
            let mut surface = Surface::new(document.pixel_format());
            surface.try_write_region(Rect::from_xywh(0, 0, width, height), &pixels).map_err(|e| bad(OPEN, format!("cannot allocate generated canvas: {e}")))?;
            surface.prune();
            let mut layer = Layer::new(if count > 1 { frame_name(name, index, count, *delay) } else { "Generated image".to_owned() }, LayerContent::Raster(surface));
            layer.visible = index == 0;
            ids.push(layer.id);
            document.layers.push(layer);
        }
    }
    let index = s.add_document(document, None);
    let delays: Vec<u32> = frames.iter().map(|(_, delay)| *delay).collect();
    Ok(json!({
        "document": index,
        "layer": ids.first().map(|id| id.0),
        "layers": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
        "frames": count,
        "delays": delays,
        "width": canvas_width,
        "height": canvas_height,
        "sheet": sheet.then(|| json!({"columns": columns, "rows": rows, "frameWidth": width, "frameHeight": height})),
    }))
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
            params: r#"{"target":json (the unmodified ai.capture target, or the `target` a previous grown placement returned),"data":text (base64 PNG/JPEG/WebP, or an animated GIF/WebP),"name":text?,"fit":"exact"|"grow"?,"grid":{"columns":1..16,"rows":1..16,"count":u32?}? (a still image laid out as a flipbook sheet: cut into its first `count` cells, read left to right then top to bottom)} (new pixel layers at captured bounds and coverage: one for a still image, one per frame for an animation, the first shown; "grow" first enlarges the whole document (Image Size, Bicubic Smoother, at most 16384 px per side and 100 MP) so a result with more pixels than the rectangle keeps them, and returns the new `target` and `grown`; rejects a different active document or changed canvas)"#,
            enabled: has_doc,
            run: apply,
            journal: true,
        },
        CommandSpec {
            id: OPEN,
            label: "Open Generated Image",
            menu: &[],
            shortcut: None,
            params: r#"{"data":text (base64 PNG/JPEG/WebP, or an animated GIF/WebP),"name":text?,"width":u32?,"height":u32?,"frames":"layers"|"sheet"?,"columns":1..256?,"grid":{"columns":1..16,"rows":1..16,"count":u32?}? (a still image laid out as a flipbook sheet: cut into its first `count` cells first)} (new independent sRGB document; width/height size each frame, provide both or neither; an animation opens one layer per frame (the first shown) or, with "sheet", one flipbook layer with the frames in a grid, `columns` or the smallest square)"#,
            enabled: crate::commands::always,
            run: open,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "generative_cmds_tests.rs"]
mod tests;
