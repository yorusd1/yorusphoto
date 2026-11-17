//! yorusphoto: flipbook sheets for game effects. A flipbook is one texture holding an animation's
//! frames in a grid, read left to right and top to bottom (Roblox ParticleEmitter flipbooks are
//! square 2x2, 4x4 or 8x8 grids; other engines read any grid). Make Flipbook Sheet lays frame
//! layers out as a sheet; Split Flipbook Sheet turns a sheet back into one layer per frame. Both
//! open a new document, so the source is never changed. The Animation Preview (the UI's) plays an
//! animation from frames rendered here ([`preview_frames`]).

use photocraft_cms::{Builtin, Intent, Transform};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const MAKE_SHEET: &str = "layer.flipbook.makeSheet";
pub const SPLIT_SHEET: &str = "layer.flipbook.splitSheet";

/// Most frames one sheet holds (an 16x16 grid).
pub const MAX_FRAMES: usize = 256;
/// Bounds of a sheet or of the frames split from one, so no request allocates without limit.
const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 100_000_000;

fn bad(command: &str, message: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: command.into(), msg: message.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn checked(command: &str, width: u32, height: u32) -> Result<usize> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || pixels > MAX_PIXELS {
        return Err(bad(command, format!("the result must be 1 to {MAX_SIDE} pixels per side and at most {MAX_PIXELS} pixels")));
    }
    usize::try_from(pixels).map_err(|_| bad(command, "the result does not fit in memory"))
}

/// A whole number parameter in `range`, or `None` when absent.
fn whole(p: &Value, key: &str, range: std::ops::RangeInclusive<usize>, command: &str) -> Result<Option<usize>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| range.contains(n))
            .map(Some)
            .ok_or_else(|| bad(command, format!("`{key}` must be a whole number from {} to {}", range.start(), range.end()))),
    }
}

/// The sheet's (columns, rows) for `count` frames. `grid`: `fit` (default) is the smallest square
/// number of columns with as many rows as needed; `square` keeps rows equal to columns; `roblox`
/// is the smallest of 2x2, 4x4 and 8x8 that holds the frames. `columns` (and `rows`) set it exactly.
fn grid(p: &Value, count: usize, command: &str) -> Result<(usize, usize)> {
    let columns = whole(p, "columns", 1..=MAX_FRAMES, command)?;
    let rows = whole(p, "rows", 1..=MAX_FRAMES, command)?;
    let side = {
        let mut side = 1usize;
        while side.saturating_mul(side) < count {
            side += 1;
        }
        side
    };
    let (columns, rows) = match (columns, rows) {
        (Some(c), Some(r)) => (c, r),
        (Some(c), None) => (c, count.div_ceil(c)),
        (None, Some(r)) => (count.div_ceil(r), r),
        (None, None) => match p.get("grid").and_then(Value::as_str).unwrap_or("fit") {
            "fit" => (side, count.div_ceil(side)),
            "square" => (side, side),
            "roblox" => {
                let side = [2usize, 4, 8].into_iter().find(|n| n * n >= count).ok_or_else(|| bad(command, "Roblox flipbooks hold at most 64 frames (8x8)"))?;
                (side, side)
            }
            _ => return Err(bad(command, "`grid` must be \"fit\", \"square\" or \"roblox\"")),
        },
    };
    if columns.saturating_mul(rows) < count {
        return Err(bad(command, format!("a {columns}x{rows} grid holds {} frames, not {count}", columns * rows)));
    }
    Ok((columns, rows))
}

/// `layer` alone, as the document would show it (opacity, mask, effects), converted to sRGB.
fn layer_rgba(doc: &Document, layer: &Layer) -> Result<Vec<f32>> {
    let mut single = doc.clone();
    let mut shown = layer.clone();
    shown.visible = true;
    single.layers = vec![shown];
    let area = doc.bounds();
    crate::allocation::checkpoint("rendering a flipbook frame")?;
    let buffer = photocraft_compose::render(&single, area);
    to_srgb(doc, buffer.px.iter().flatten().copied().collect(), MAKE_SHEET)
}

fn to_srgb(doc: &Document, mut rgba: Vec<f32>, command: &str) -> Result<Vec<f32>> {
    let profile = crate::color_cmds::composite_profile(doc);
    let transform = Transform::new(&profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .map_err(|e| bad(command, format!("cannot convert to sRGB: {e}")))?;
    transform.apply(&mut rgba, 4);
    Ok(rgba)
}

/// A new sRGB RGB document of `size` with `depth` matching the source (8 or 16 bits).
fn new_document(name: String, size: Size, source_depth: SampleType) -> Document {
    let depth = if source_depth == SampleType::U8 { SampleType::U8 } else { SampleType::U16 };
    let mut document = Document::new(name, size, ColorMode::Rgb, depth);
    document.icc_profile = Some(std::sync::Arc::new(Builtin::Srgb.profile().to_bytes().to_vec()));
    document
}

/// The frame layers: `layers` (ids, in the order given) or the selected layers bottom to top.
fn frame_layers(s: &Session, p: &Value) -> Result<Vec<LayerId>> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let ids = match p.get("layers") {
        None | Some(Value::Null) => st.selected_layers(),
        Some(Value::Array(values)) => {
            let mut ids = Vec::with_capacity(values.len());
            for value in values {
                let id = value.as_u64().map(LayerId).filter(|id| st.doc.layer(*id).is_some()).ok_or_else(|| bad(MAKE_SHEET, "`layers` must list layer ids of this document"))?;
                ids.push(id);
            }
            ids
        }
        Some(_) => return Err(bad(MAKE_SHEET, "`layers` must be an array of layer ids")),
    };
    if ids.len() < 2 {
        return Err(bad(MAKE_SHEET, "select two or more frame layers (bottom to top), or pass `layers`"));
    }
    if ids.len() > MAX_FRAMES {
        return Err(bad(MAKE_SHEET, format!("a sheet holds at most {MAX_FRAMES} frames")));
    }
    Ok(ids)
}

/// Layer › Flipbook › Make Flipbook Sheet: each frame layer rendered alone at the canvas size,
/// laid out left to right, top to bottom, in a new document.
fn make_sheet(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = frame_layers(s, p)?;
    let (columns, rows) = grid(p, ids.len(), MAKE_SHEET)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.clone();
    let (width, height) = (doc.size.width, doc.size.height);
    let too_large = || bad(MAKE_SHEET, "the flipbook sheet is too large");
    let sheet_width = width.checked_mul(u32::try_from(columns).map_err(|_| too_large())?).ok_or_else(too_large)?;
    let sheet_height = height.checked_mul(u32::try_from(rows).map_err(|_| too_large())?).ok_or_else(too_large)?;
    checked(MAKE_SHEET, sheet_width, sheet_height)?;
    let mut document = new_document(format!("{} flipbook", doc.name), Size::new(sheet_width, sheet_height), doc.depth);
    let mut surface = Surface::new(document.pixel_format());
    for (index, id) in ids.iter().enumerate() {
        let layer = doc.layer(*id).ok_or_else(|| bad(MAKE_SHEET, "a frame layer was removed"))?;
        let rgba = layer_rgba(&doc, layer)?;
        let (column, row) = (index % columns, index / columns);
        let x = i32::try_from(column).ok().and_then(|c| c.checked_mul(i32::try_from(width).ok()?)).ok_or_else(too_large)?;
        let y = i32::try_from(row).ok().and_then(|r| r.checked_mul(i32::try_from(height).ok()?)).ok_or_else(too_large)?;
        surface.try_write_region(Rect::from_xywh(x, y, width, height), &rgba).map_err(|e| bad(MAKE_SHEET, format!("cannot allocate the sheet: {e}")))?;
    }
    surface.prune();
    let layer = Layer::new(format!("Flipbook {columns}x{rows}"), LayerContent::Raster(surface));
    let layer_id = layer.id.0;
    document.layers.push(layer);
    let index = s.add_document(document, None);
    Ok(json!({"document": index, "layer": layer_id, "frames": ids.len(), "columns": columns, "rows": rows, "frameWidth": width, "frameHeight": height, "width": sheet_width, "height": sheet_height}))
}

/// Layer › Flipbook › Split Flipbook Sheet: the visible document cut into `columns` x `rows`
/// cells, one layer per frame (the first shown) in a new document of the cell size. Empty cells
/// at the end are left out unless `keepEmpty`.
fn split_sheet(s: &mut Session, p: &Value) -> Result<Value> {
    let columns = whole(p, "columns", 1..=MAX_FRAMES, SPLIT_SHEET)?.ok_or_else(|| bad(SPLIT_SHEET, "`columns` is required"))?;
    let rows = whole(p, "rows", 1..=MAX_FRAMES, SPLIT_SHEET)?.unwrap_or(columns);
    let keep_empty = p.get("keepEmpty").and_then(Value::as_bool).unwrap_or(false);
    if columns.saturating_mul(rows) > MAX_FRAMES {
        return Err(bad(SPLIT_SHEET, format!("a sheet holds at most {MAX_FRAMES} frames")));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.clone();
    let (cell_width, cell_height) = (doc.size.width / u32::try_from(columns).unwrap_or(u32::MAX), doc.size.height / u32::try_from(rows).unwrap_or(u32::MAX));
    if cell_width == 0 || cell_height == 0 {
        return Err(bad(SPLIT_SHEET, "the grid has more cells than the document has pixels"));
    }
    let count = checked(SPLIT_SHEET, cell_width, cell_height)?;
    crate::allocation::checkpoint("splitting the flipbook sheet")?;
    let mut frames: Vec<Vec<f32>> = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            let x = i32::try_from(column as u64 * u64::from(cell_width)).map_err(|_| bad(SPLIT_SHEET, "the sheet is too large"))?;
            let y = i32::try_from(row as u64 * u64::from(cell_height)).map_err(|_| bad(SPLIT_SHEET, "the sheet is too large"))?;
            let buffer = photocraft_compose::render(&doc, Rect::from_xywh(x, y, cell_width, cell_height));
            let rgba = to_srgb(&doc, buffer.px.iter().flatten().copied().collect(), SPLIT_SHEET)?;
            if rgba.len() != count * 4 {
                return Err(bad(SPLIT_SHEET, "the cell could not be read"));
            }
            frames.push(rgba);
        }
    }
    if !keep_empty {
        while frames.len() > 1 && frames.last().is_some_and(|f| f.as_chunks::<4>().0.iter().all(|px| px[3] <= 0.0)) {
            frames.pop();
        }
    }
    let total = frames.len();
    let mut document = new_document(format!("{} frames", doc.name), Size::new(cell_width, cell_height), doc.depth);
    let mut ids = Vec::with_capacity(total);
    for (index, rgba) in frames.into_iter().enumerate() {
        let mut surface = Surface::new(document.pixel_format());
        surface.try_write_region(Rect::from_xywh(0, 0, cell_width, cell_height), &rgba).map_err(|e| bad(SPLIT_SHEET, format!("cannot allocate a frame: {e}")))?;
        surface.prune();
        let mut layer = Layer::new(format!("Frame {} of {total}", index + 1), LayerContent::Raster(surface));
        layer.visible = index == 0;
        ids.push(layer.id.0);
        document.layers.push(layer);
    }
    let index = s.add_document(document, None);
    Ok(json!({"document": index, "layers": ids, "frames": total, "frameWidth": cell_width, "frameHeight": cell_height}))
}

// ---- yorusphoto: the frames the Animation Preview plays ----

/// The command the Animation Preview's errors name (the window is the UI's; its frames are made here).
const PREVIEW: &str = "layer.flipbook.preview";
/// Rows of a frame rendered at a time, so a large canvas never takes a full-size copy.
const PREVIEW_BAND: u32 = 256;
/// The grid a sheet layer previews with when its name gives none: Roblox's smallest flipbook.
pub const DEFAULT_SHEET_GRID: (usize, usize) = (2, 2);

/// Where an animation's frames come from, for the Animation Preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameSource {
    /// One layer per frame, in playing order.
    Layers(Vec<LayerId>),
    /// A flipbook sheet: `layer` cut into `columns` x `rows` cells of the canvas, read left to
    /// right and top to bottom.
    Sheet { layer: LayerId, columns: usize, rows: usize },
}

/// One frame for a preview: `width` x `height` 8-bit sRGB pixels, straight alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An animation's frames for a preview, and the canvas area each one shows.
#[derive(Clone, Debug)]
pub struct Preview {
    pub frames: Vec<PreviewFrame>,
    /// The area of the canvas a frame shows: the area the frame layers' pixels share (the whole
    /// canvas when they have none), or a sheet's first cell.
    pub area: Rect,
    pub canvas: Rect,
}

/// The time a frame's layer name gives it: `… · 100 ms`, as generated animations are named.
pub fn frame_delay(name: &str) -> Option<u32> {
    let rest = name.strip_suffix(" ms")?;
    rest.rsplit(" · ").next()?.trim().parse().ok().filter(|ms| *ms > 0)
}

/// A frame layer's (number, count, animation name): `Torch · frame 2 of 8 · 100 ms`, as generated
/// animations are named, or `Frame 2 of 8`, as Split Flipbook Sheet names them.
fn frame_number(name: &str) -> Option<(usize, usize, &str)> {
    let (base, rest) = match name.split_once(" · frame ") {
        Some(parts) => parts,
        None => ("", name.strip_prefix("Frame ")?),
    };
    let (number, rest) = rest.split_once(" of ")?;
    let count = rest.split(" · ").next()?;
    let (number, count) = (number.trim().parse::<usize>().ok()?, count.trim().parse::<usize>().ok()?);
    (1..=count).contains(&number).then_some((number, count, base))
}

/// The grid a sheet layer's name gives: `Torch · flipbook 4x4`, `Flipbook 2x2`.
pub fn sheet_grid_in_name(name: &str) -> Option<(usize, usize)> {
    let lower = name.to_ascii_lowercase();
    let (_, grid) = lower.rsplit_once("flipbook ")?;
    let (columns, rows) = grid.split_once('x')?;
    let rows = rows.split(|c: char| !c.is_ascii_digit()).next()?;
    let (columns, rows): (usize, usize) = (columns.trim().parse().ok()?, rows.parse().ok()?);
    ((1..=MAX_FRAMES).contains(&columns) && (1..=MAX_FRAMES).contains(&rows) && columns * rows <= MAX_FRAMES).then_some((columns, rows))
}

/// The (columns, rows) Make Flipbook Sheet lays `count` frames out on with `grid` (`fit`,
/// `square` or `roblox`), or `None` when that grid can't hold them.
pub fn sheet_layout(count: usize, grid_mode: &str) -> Option<(usize, usize)> {
    grid(&json!({"grid": grid_mode}), count, MAKE_SHEET).ok()
}

/// The layers holding `id`: its siblings, itself among them.
fn siblings_of(layers: &[Layer], id: LayerId) -> Option<&[Layer]> {
    if layers.iter().any(|layer| layer.id == id) {
        return Some(layers);
    }
    layers.iter().filter_map(Layer::children).find_map(|children| siblings_of(children, id))
}

/// The animation to preview in `doc`: the selected layers when two or more are (bottom to top);
/// else the frames the active layer belongs to (the run of its siblings named as frames of the
/// same animation, in frame order); else the active pixel layer as a flipbook sheet, its grid
/// from its name ([`DEFAULT_SHEET_GRID`] when the name gives none).
pub fn find_animation(doc: &Document, selected: &[LayerId], active: Option<LayerId>) -> Option<FrameSource> {
    if selected.len() >= 2 {
        return Some(FrameSource::Layers(selected.iter().take(MAX_FRAMES).copied().collect()));
    }
    let layer = doc.layer(active?)?;
    if let Some((_, count, base)) = frame_number(&layer.name)
        && let Some(siblings) = siblings_of(&doc.layers, layer.id)
        && let Some(at) = siblings.iter().position(|sibling| sibling.id == layer.id)
    {
        let same = |sibling: &Layer| frame_number(&sibling.name).is_some_and(|(_, c, b)| c == count && b == base);
        let start = siblings[..at].iter().rposition(|sibling| !same(sibling)).map_or(0, |i| i + 1);
        let end = siblings[at..].iter().position(|sibling| !same(sibling)).map_or(siblings.len(), |i| at + i);
        let mut frames: Vec<(usize, LayerId)> = siblings[start..end].iter().filter_map(|s| frame_number(&s.name).map(|(n, _, _)| (n, s.id))).collect();
        frames.sort_by_key(|(number, _)| *number);
        frames.dedup_by_key(|(number, _)| *number);
        if frames.len() >= 2 {
            return Some(FrameSource::Layers(frames.into_iter().take(MAX_FRAMES).map(|(_, id)| id).collect()));
        }
    }
    // A sheet is a layer with pixels.
    layer.surface()?;
    let (columns, rows) = sheet_grid_in_name(&layer.name).unwrap_or(DEFAULT_SHEET_GRID);
    Some(FrameSource::Sheet { layer: layer.id, columns, rows })
}

/// The frames of `source` rendered for a preview, each at most `max_side` pixels on its longer
/// side. Frame layers are each rendered alone (opacity, mask and effects as the document shows
/// them) over `area`, or the whole canvas: a GIF's frame, its transparent margin included, as
/// [`MAKE_SHEET`] lays it in a cell. A sheet is cut into its cells of the canvas (`area` unused),
/// trailing empty cells left out.
pub fn preview_frames(doc: &Document, source: &FrameSource, area: Option<Rect>, max_side: u32) -> Result<Preview> {
    let canvas = doc.bounds();
    let mut base = doc.clone();
    base.layers.clear();
    let profile = crate::color_cmds::composite_profile(doc);
    let transform = Transform::new(&profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .map_err(|e| bad(PREVIEW, format!("cannot convert to sRGB: {e}")))?;
    let max_side = max_side.clamp(1, MAX_SIDE);
    match source {
        FrameSource::Layers(ids) => {
            if ids.is_empty() || ids.len() > MAX_FRAMES {
                return Err(bad(PREVIEW, format!("an animation has 1 to {MAX_FRAMES} frames")));
            }
            let layers = ids.iter().map(|id| doc.layer(*id).ok_or_else(|| bad(PREVIEW, "a frame layer was removed"))).collect::<Result<Vec<_>>>()?;
            let area = area.map_or(canvas, |area| area.intersect(&canvas));
            if area.is_empty() {
                return Err(bad(PREVIEW, "the frames' area is outside the canvas"));
            }
            let frames = layers.iter().map(|layer| render_shrunk(&base, layer, area, max_side, &transform)).collect::<Result<_>>()?;
            Ok(Preview { frames, area, canvas })
        }
        FrameSource::Sheet { layer, columns, rows } => {
            let (columns, rows) = (*columns, *rows);
            if !(1..=MAX_FRAMES).contains(&columns) || !(1..=MAX_FRAMES).contains(&rows) || columns * rows > MAX_FRAMES {
                return Err(bad(PREVIEW, format!("a sheet holds at most {MAX_FRAMES} frames")));
            }
            let layer = doc.layer(*layer).ok_or_else(|| bad(PREVIEW, "the sheet layer was removed"))?;
            let side = |count: usize, length: u32| length / u32::try_from(count).unwrap_or(u32::MAX);
            let (cell_width, cell_height) = (side(columns, canvas.width()), side(rows, canvas.height()));
            if cell_width == 0 || cell_height == 0 {
                return Err(bad(PREVIEW, "the grid has more cells than the canvas has pixels"));
            }
            let offset = |n: usize, length: u32| i32::try_from(n as u64 * u64::from(length)).map_err(|_| bad(PREVIEW, "the sheet is too large"));
            let mut frames = Vec::with_capacity(columns * rows);
            for index in 0..columns * rows {
                let x = canvas.x0.saturating_add(offset(index % columns, cell_width)?);
                let y = canvas.y0.saturating_add(offset(index / columns, cell_height)?);
                frames.push(render_shrunk(&base, layer, Rect::from_xywh(x, y, cell_width, cell_height), max_side, &transform)?);
            }
            while frames.len() > 1 && frames.last().is_some_and(|frame| frame.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 0)) {
                frames.pop();
            }
            Ok(Preview { frames, area: Rect::from_xywh(canvas.x0, canvas.y0, cell_width, cell_height), canvas })
        }
    }
}

/// The size `width` x `height` shrinks to with at most `max_side` pixels on its longer side.
fn shrunk_size(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let scale = (f64::from(max_side) / f64::from(width.max(height).max(1))).min(1.0);
    let side = |n: u32| ((f64::from(n) * scale).round() as u32).max(1);
    (side(width), side(height))
}

/// `layer` alone over `area` as 8-bit sRGB, area-averaged (premultiplied) down to at most
/// `max_side` pixels on its longer side. It renders in bands of `PREVIEW_BAND` rows.
fn render_shrunk(base: &Document, layer: &Layer, area: Rect, max_side: u32, transform: &Transform) -> Result<PreviewFrame> {
    let (width, height) = (area.width(), area.height());
    let (out_width, out_height) = shrunk_size(width, height, max_side);
    let (columns, rows) = (out_width as usize, out_height as usize);
    let mut single = base.clone();
    let mut shown = layer.clone();
    shown.visible = true;
    single.layers = vec![shown];
    // Premultiplied red, green and blue, then alpha and the pixel count, per preview pixel.
    let mut sums = vec![[0.0f64; 5]; columns * rows];
    let mut top = 0;
    while top < height {
        let band = (height - top).min(PREVIEW_BAND);
        crate::allocation::checkpoint("rendering an animation preview")?;
        let y = area.y0.saturating_add(i32::try_from(top).unwrap_or(i32::MAX));
        let buffer = photocraft_compose::render(&single, Rect::from_xywh(area.x0, y, width, band));
        let mut rgba: Vec<f32> = buffer.px.iter().flatten().copied().collect();
        transform.apply(&mut rgba, 4);
        for (index, px) in rgba.as_chunks::<4>().0.iter().enumerate() {
            let (x, line) = (index % width as usize, top as usize + index / width as usize);
            let sum = &mut sums[line * rows / height as usize * columns + x * columns / width as usize];
            let alpha = f64::from(px[3].clamp(0.0, 1.0));
            sum[0] += f64::from(px[0]) * alpha;
            sum[1] += f64::from(px[1]) * alpha;
            sum[2] += f64::from(px[2]) * alpha;
            sum[3] += alpha;
            sum[4] += 1.0;
        }
        top += band;
    }
    let byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = Vec::with_capacity(columns * rows * 4);
    for [red, green, blue, alpha, count] in sums {
        let (r, g, b) = if alpha > 0.0 { (red / alpha, green / alpha, blue / alpha) } else { (0.0, 0.0, 0.0) };
        out.extend([byte(r), byte(g), byte(b), byte(if count > 0.0 { alpha / count } else { 0.0 })]);
    }
    Ok(PreviewFrame { width: out_width, height: out_height, rgba: out })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: MAKE_SHEET,
            label: "Make Flipbook Sheet…",
            menu: &["Layer", "Flipbook"],
            shortcut: None,
            params: r#"{"layers":[id]? (frame layers in order; default the selected layers, bottom to top),"grid":"fit"|"square"|"roblox"? ("roblox": 2x2, 4x4 or 8x8),"columns":1..256?,"rows":1..256?} (each frame layer rendered alone at the canvas size, laid out left to right and top to bottom in a new sRGB document; returns its columns, rows and frame size)"#,
            enabled: has_doc,
            run: make_sheet,
            journal: false,
        },
        CommandSpec {
            id: SPLIT_SHEET,
            label: "Split Flipbook Sheet…",
            menu: &["Layer", "Flipbook"],
            shortcut: None,
            params: r#"{"columns":1..256,"rows":1..256? (default columns),"keepEmpty":bool?} (the visible document cut into a grid, one layer per cell in a new document of the cell size, the first shown; trailing empty cells are left out unless keepEmpty)"#,
            enabled: has_doc,
            run: split_sheet,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_document() -> (Session, Vec<u64>) {
        let mut session = Session::new();
        session.execute("file.new", json!({"width": 4, "height": 3, "mode": "rgb", "depth": 8, "background": "transparent"})).unwrap();
        let mut ids = Vec::new();
        for (index, colour) in [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]].iter().enumerate() {
            let id = session
                .edit("frame", |doc, active| {
                    let mut layer = Layer::raster(format!("Frame {index}"), doc.pixel_format());
                    layer.visible = index == 0;
                    layer.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 3), colour);
                    let id = doc.insert_above(*active, layer);
                    *active = Some(id);
                    Ok(id)
                })
                .unwrap();
            ids.push(id.0);
        }
        (session, ids)
    }

    #[test]
    fn frame_layers_make_a_sheet_and_the_sheet_splits_back_into_the_same_frames() {
        let (mut session, ids) = frames_document();
        let sheet = session.execute(MAKE_SHEET, json!({"layers": ids, "grid": "roblox"})).unwrap();
        assert_eq!((sheet["columns"].as_u64(), sheet["rows"].as_u64()), (Some(2), Some(2)));
        assert_eq!((sheet["width"].as_u64(), sheet["height"].as_u64()), (Some(8), Some(6)));
        let doc = &session.active().unwrap().doc;
        let surface = doc.layers[0].surface().unwrap();
        assert!(surface.rgba(1, 1)[0] > 0.99, "frame 1 at the top left, rendered although hidden layers follow");
        assert!(surface.rgba(5, 1)[1] > 0.99, "frame 2 to its right, hidden in the source but still a frame");
        assert!(surface.rgba(1, 4)[2] > 0.99, "frame 3 starts the second row");
        assert!(surface.rgba(5, 4)[3] < 0.01, "the fourth cell stays empty");
        let split = session.execute(SPLIT_SHEET, json!({"columns": 2, "rows": 2})).unwrap();
        assert_eq!(split["frames"], 3, "the trailing empty cell is left out");
        let doc = &session.active().unwrap().doc;
        assert_eq!((doc.size, doc.layers.len()), (Size::new(4, 3), 3));
        assert!(doc.layers[0].visible && !doc.layers[1].visible);
        assert!(doc.layers[2].surface().unwrap().rgba(2, 2)[2] > 0.99);
        let kept = session.execute(SPLIT_SHEET, json!({"columns": 2, "keepEmpty": true})).unwrap();
        assert_eq!(kept["frames"], 4);
    }

    #[test]
    fn grids_follow_the_requested_layout_and_refuse_impossible_ones() {
        let p = |v: Value| v;
        assert_eq!(grid(&p(json!({})), 5, MAKE_SHEET).unwrap(), (3, 2));
        assert_eq!(grid(&p(json!({"grid": "square"})), 5, MAKE_SHEET).unwrap(), (3, 3));
        assert_eq!(grid(&p(json!({"grid": "roblox"})), 5, MAKE_SHEET).unwrap(), (4, 4));
        assert_eq!(grid(&p(json!({"grid": "roblox"})), 64, MAKE_SHEET).unwrap(), (8, 8));
        assert!(grid(&p(json!({"grid": "roblox"})), 65, MAKE_SHEET).is_err());
        assert_eq!(grid(&p(json!({"columns": 5})), 7, MAKE_SHEET).unwrap(), (5, 2));
        assert!(grid(&p(json!({"columns": 2, "rows": 2})), 5, MAKE_SHEET).is_err());
        assert!(grid(&p(json!({"grid": "hex"})), 4, MAKE_SHEET).is_err());
        assert!(grid(&p(json!({"columns": 0})), 4, MAKE_SHEET).is_err());
    }

    /// An 8x6 transparent canvas with one frame layer per name, each painted red, green, blue or
    /// yellow in the 4x4 square at (2, 1), the first shown.
    fn named_frames(names: &[&str]) -> (Session, Vec<LayerId>) {
        let mut session = Session::new();
        session.execute("file.new", json!({"width": 8, "height": 6, "mode": "rgb", "depth": 8, "background": "transparent"})).unwrap();
        let colours = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0]];
        let mut ids = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let id = session
                .edit("frame", |doc, active| {
                    let mut layer = Layer::raster(name.to_string(), doc.pixel_format());
                    layer.visible = index == 0;
                    layer.surface_mut().unwrap().fill_rect(Rect::new(2, 1, 6, 5), &colours[index % 4]);
                    let id = doc.insert_above(*active, layer);
                    *active = Some(id);
                    Ok(id)
                })
                .unwrap();
            ids.push(id);
        }
        (session, ids)
    }

    #[test]
    fn frame_and_sheet_names_are_read() {
        assert_eq!(frame_delay("Torch · frame 2 of 4 · 100 ms"), Some(100));
        assert_eq!(frame_delay("Torch · frame 2 of 4"), None);
        assert_eq!(frame_number("Torch · frame 2 of 4 · 100 ms"), Some((2, 4, "Torch")));
        assert_eq!(frame_number("Frame 3 of 3"), Some((3, 3, "")));
        assert_eq!(frame_number("Torch · frame 5 of 4"), None);
        assert_eq!(frame_number("Background"), None);
        assert_eq!(sheet_grid_in_name("Smoke · flipbook 2x2"), Some((2, 2)));
        assert_eq!(sheet_grid_in_name("Flipbook 4x3"), Some((4, 3)));
        assert_eq!(sheet_grid_in_name("Flipbook 17x17"), None, "more than 256 cells");
        assert_eq!(sheet_grid_in_name("Layer 1"), None);
        assert_eq!(sheet_layout(5, "roblox"), Some((4, 4)));
        assert_eq!(sheet_layout(5, "fit"), Some((3, 2)));
        assert_eq!(sheet_layout(65, "roblox"), None);
    }

    #[test]
    fn the_animation_is_found_from_the_selection_a_frame_or_a_sheet() {
        let names = ["Torch · frame 1 of 3 · 80 ms", "Torch · frame 2 of 3 · 80 ms", "Torch · frame 3 of 3 · 80 ms"];
        let (session, ids) = named_frames(&names);
        let doc = &session.active().unwrap().doc;
        assert_eq!(find_animation(doc, &[ids[1]], Some(ids[1])), Some(FrameSource::Layers(ids.clone())), "the active frame finds its siblings, in frame order");
        assert_eq!(find_animation(doc, &[ids[2], ids[0]], Some(ids[0])), Some(FrameSource::Layers(vec![ids[2], ids[0]])), "two or more selected layers are the frames");
        let background = doc.layers[0].id;
        assert_eq!(
            find_animation(doc, &[background], Some(background)),
            Some(FrameSource::Sheet { layer: background, columns: 2, rows: 2 }),
            "a pixel layer that isn't a frame previews as a sheet"
        );
        assert_eq!(find_animation(doc, &[], None), None);
    }

    #[test]
    fn previews_show_whole_frames_or_an_area_and_cut_sheets_into_cells() {
        let (mut session, ids) = named_frames(&["Torch · frame 1 of 3", "Torch · frame 2 of 3", "Torch · frame 3 of 3"]);
        let doc = session.active().unwrap().doc.clone();
        let whole = preview_frames(&doc, &FrameSource::Layers(ids.clone()), None, 512).unwrap();
        assert_eq!(whole.area, doc.bounds(), "a GIF's whole frame");
        assert_eq!((whole.frames[0].width, whole.frames[0].height), (8, 6));
        assert_eq!(whole.frames[0].rgba[3], 0, "its transparent margin stays");
        let at = (8 + 2) * 4;
        assert_eq!(&whole.frames[0].rgba[at..at + 4], &[255, 0, 0, 255]);
        let preview = preview_frames(&doc, &FrameSource::Layers(ids.clone()), Some(Rect::new(2, 1, 6, 5)), 512).unwrap();
        assert_eq!(preview.area, Rect::new(2, 1, 6, 5), "frames placed in a selected area show it");
        assert_eq!(preview.frames.len(), 3);
        assert_eq!((preview.frames[0].width, preview.frames[0].height), (4, 4));
        assert_eq!(&preview.frames[0].rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&preview.frames[2].rgba[..4], &[0, 0, 255, 255], "hidden frames render too");
        assert!(preview_frames(&doc, &FrameSource::Layers(ids.clone()), Some(Rect::new(20, 20, 30, 30)), 64).is_err(), "an area off the canvas");
        let small = preview_frames(&doc, &FrameSource::Layers(ids.clone()), Some(Rect::new(2, 1, 6, 5)), 2).unwrap();
        assert_eq!((small.frames[1].width, small.frames[1].height), (2, 2));
        assert_eq!(&small.frames[1].rgba[..4], &[0, 255, 0, 255]);
        let numbers: Vec<u64> = ids.iter().map(|id| id.0).collect();
        session.execute(MAKE_SHEET, json!({"layers": numbers, "grid": "roblox"})).unwrap();
        let sheet_doc = session.active().unwrap().doc.clone();
        let sheet = sheet_doc.layers[0].id;
        let found = find_animation(&sheet_doc, &[sheet], Some(sheet));
        assert_eq!(found, Some(FrameSource::Sheet { layer: sheet, columns: 2, rows: 2 }), "the sheet's name gives its grid");
        let cells = preview_frames(&sheet_doc, &found.unwrap(), None, 512).unwrap();
        assert_eq!(cells.frames.len(), 3, "the empty fourth cell is left out");
        assert_eq!((cells.frames[0].width, cells.frames[0].height), (8, 6), "a cell is a whole frame canvas");
        let at = (8 + 2) * 4;
        assert_eq!(&cells.frames[1].rgba[at..at + 4], &[0, 255, 0, 255]);
        assert!(preview_frames(&sheet_doc, &FrameSource::Sheet { layer: sheet, columns: 20, rows: 20 }, None, 64).is_err());
    }

    #[test]
    fn bad_requests_never_open_a_document() {
        let (mut session, ids) = frames_document();
        let open = session.documents().len();
        for params in [json!({"layers": [ids[0]]}), json!({"layers": "all"}), json!({"layers": [999_999, ids[0]]})] {
            assert!(session.execute(MAKE_SHEET, params).is_err());
        }
        for params in [json!({}), json!({"columns": 0}), json!({"columns": 300}), json!({"columns": 16, "rows": 17}), json!({"columns": 5, "rows": 1})] {
            assert!(session.execute(SPLIT_SHEET, params).is_err());
        }
        assert_eq!(session.documents().len(), open);
        let mut empty = Session::new();
        assert!(!empty.is_enabled(MAKE_SHEET));
        assert!(empty.execute(SPLIT_SHEET, json!({"columns": 2})).is_err());
    }
}
