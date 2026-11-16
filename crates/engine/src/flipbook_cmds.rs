//! yorusphoto: flipbook sheets for game effects. A flipbook is one texture holding an animation's
//! frames in a grid, read left to right and top to bottom (Roblox ParticleEmitter flipbooks are
//! square 2x2, 4x4 or 8x8 grids; other engines read any grid). Make Flipbook Sheet lays frame
//! layers out as a sheet; Split Flipbook Sheet turns a sheet back into one layer per frame. Both
//! open a new document, so the source is never changed.

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
