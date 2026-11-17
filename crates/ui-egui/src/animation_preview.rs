//! yorusphoto: the Animation Preview window. An animation (one layer per frame, as a Codex GIF
//! arrives, or a flipbook sheet) plays on the left while the flipbook sheet it makes is laid out
//! on the right; the frame on show is lit in both, and in the strip of every frame below. It opens
//! by itself when Codex returns an animation, and from Layer › Flipbook › Preview Animation….
//! The engine renders the frames (`flipbook_cmds::preview_frames`), again when their layers change.

use egui::{Color32, Rect as Area, RichText, Sense, Stroke, StrokeKind, vec2};
use photocraft_doc::{Document, LayerId};
use photocraft_engine::flipbook_cmds::{self, FrameSource, PreviewFrame};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Layer › Flipbook › Preview Animation…, and the control API's command.
pub const COMMAND: &str = "layer.flipbook.preview";

/// Pixels all of an animation's preview frames hold together (8-bit RGBA), so even a 256-frame
/// sheet stays light: 512 pixels a side for up to 30 frames, 176 for 256.
const FRAME_BUDGET: f64 = 8_000_000.0;
/// Seconds between checks that the frames' layers are unchanged.
const RECHECK: f64 = 0.5;
/// The frame rate when the animation gives none (a sheet, or frames with no time in their names).
const DEFAULT_FPS: f32 = 10.0;

/// The grid the flipbook sheet is shown, and made, with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grid {
    Fit,
    Square,
    Roblox,
}

impl Grid {
    fn key(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Square => "square",
            Self::Roblox => "roblox",
        }
    }
}

/// What shows through transparent pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backdrop {
    Checker,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    MakeSheet,
    Split,
    Regrid,
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Previous,
    Next,
}

/// The open preview.
pub struct Preview {
    /// The document the frames are in (its `DocId`).
    doc: u64,
    source: FrameSource,
    /// The part of the canvas frame layers show: where Codex placed them in a selected area.
    /// `None`: the whole canvas, a GIF's frame with its transparent margin.
    crop: Option<Rect>,
    name: String,
    /// Each frame's time in the GIF, in milliseconds (0: none given).
    delays: Vec<u32>,
    /// Frames rendered and not yet uploaded as textures.
    pending: Vec<PreviewFrame>,
    textures: Vec<egui::TextureHandle>,
    frame_size: [u32; 2],
    area: Rect,
    canvas: Rect,
    /// The frames' layers when they were rendered ([`fingerprint`]).
    rendered: u64,
    next_check: f64,
    error: Option<String>,
    current: usize,
    playing: bool,
    fps: f32,
    /// When the frame on show started (input time); `None` until playback takes a step.
    clock: Option<f64>,
    grid: Grid,
    backdrop: Backdrop,
    /// The frames' document was closed: they still play, but nothing can be made from them.
    closed: bool,
    /// The frame the strip last scrolled to.
    scrolled: Option<usize>,
    /// Where the window was last drawn: its keys work while the pointer is over it.
    window: Option<Area>,
}

impl Preview {
    fn count(&self) -> usize {
        self.textures.len().max(self.pending.len())
    }

    /// Renders the frames from `doc`; the frame on show stays where it can.
    fn render(&mut self, doc: &Document) {
        let cells = match &self.source {
            FrameSource::Layers(ids) => ids.len(),
            FrameSource::Sheet { columns, rows, .. } => columns.saturating_mul(*rows),
        };
        let max_side = ((FRAME_BUDGET / cells.max(1) as f64).sqrt() as u32).clamp(96, 512);
        self.rendered = fingerprint(doc, &self.source);
        match flipbook_cmds::preview_frames(doc, &self.source, self.crop, max_side) {
            Ok(preview) => {
                self.frame_size = preview.frames.first().map_or([1, 1], |frame| [frame.width, frame.height]);
                self.area = preview.area;
                self.canvas = preview.canvas;
                self.pending = preview.frames;
                self.error = None;
            }
            Err(error) => {
                self.pending.clear();
                self.textures.clear();
                self.error = Some(error.to_string());
            }
        }
    }

    /// Uploads the frames rendered since the last call.
    fn upload(&mut self, ctx: &egui::Context) {
        if self.pending.is_empty() {
            return;
        }
        // Small frames (pixel art) stay crisp when the preview enlarges them.
        let filter = if self.frame_size[0].max(self.frame_size[1]) <= 128 { egui::TextureOptions::NEAREST } else { egui::TextureOptions::LINEAR };
        self.textures = self
            .pending
            .drain(..)
            .enumerate()
            .map(|(index, frame)| {
                let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
                ctx.load_texture(format!("animation-preview-{index}"), image, filter)
            })
            .collect();
        self.current = self.current.min(self.textures.len().saturating_sub(1));
        self.scrolled = None;
    }

    fn toggle(&mut self) {
        self.playing = !self.playing;
        self.clock = None;
    }

    /// Shows the frame `by` frames on (back when negative), wrapping round, and pauses.
    fn step(&mut self, by: isize) {
        let count = self.count();
        if count == 0 {
            return;
        }
        self.current = (self.current as isize + by).rem_euclid(count as isize) as usize;
        self.playing = false;
        self.clock = None;
    }

    fn select(&mut self, index: usize) {
        self.current = index.min(self.count().saturating_sub(1));
        self.playing = false;
        self.clock = None;
    }

    /// Moves playback on to the input clock and asks for the repaint of the next frame.
    fn tick(&mut self, ctx: &egui::Context) {
        let count = self.textures.len();
        if !self.playing || count < 2 {
            self.clock = None;
            return;
        }
        let now = ctx.input(|input| input.time);
        let step = 1.0 / f64::from(self.fps.clamp(1.0, 60.0));
        let (current, clock) = next_frame(self.current, count, self.clock.unwrap_or(now), now, step);
        self.current = current;
        self.clock = Some(clock);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((clock + step - now).max(0.0)));
    }
}

/// Playback's frame and clock at `now`: each whole `step` since `clock` moves on a frame. A
/// repaint asked for at a frame's boundary can wake a hair before it: a microsecond counts.
fn next_frame(current: usize, count: usize, clock: f64, now: f64, step: f64) -> (usize, f64) {
    let count = count.max(1);
    let steps = ((now - clock + 1e-6) / step).floor().max(0.0);
    ((current + steps as usize % count) % count, clock + steps * step)
}

/// Identity of what the frames show: their layers' pixels (tile identity), the grid and the
/// canvas size.
fn fingerprint(doc: &Document, source: &FrameSource) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: u64| hash = (hash ^ value).wrapping_mul(0x100_0000_01b3);
    mix(u64::from(doc.size.width) << 32 | u64::from(doc.size.height));
    let pixels = |id: LayerId| doc.layer(id).and_then(|layer| layer.surface()).map_or(1, crate::app::surface_fingerprint);
    match source {
        FrameSource::Layers(ids) => {
            for id in ids {
                mix(id.0);
                mix(pixels(*id));
            }
        }
        FrameSource::Sheet { layer, columns, rows } => {
            mix(layer.0);
            mix(*columns as u64);
            mix(*rows as u64);
            mix(pixels(*layer));
        }
    }
    hash
}

/// The animation's name: its frames' (`Torch · frame 1 of 4`), its sheet's (`Torch · flipbook
/// 2x2`), else the document's.
fn animation_name(doc: &Document, source: &FrameSource) -> String {
    let first = match source {
        FrameSource::Layers(ids) => ids.first().copied(),
        FrameSource::Sheet { layer, .. } => Some(*layer),
    };
    first
        .and_then(|id| doc.layer(id))
        .and_then(|layer| layer.name.split_once(" · ").map(|(name, _)| name.trim().to_owned()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| doc.name.clone())
}

/// The times the frames' names give them (`… · 100 ms`), 0 where they give none.
fn named_delays(doc: &Document, source: &FrameSource) -> Vec<u32> {
    match source {
        FrameSource::Layers(ids) => ids.iter().map(|id| doc.layer(*id).and_then(|layer| flipbook_cmds::frame_delay(&layer.name)).unwrap_or(0)).collect(),
        FrameSource::Sheet { .. } => Vec::new(),
    }
}

/// The frame rate of `delays` (milliseconds): their median's, from 1 to 60.
fn fps_for(delays: &[u32]) -> f32 {
    let mut known: Vec<u32> = delays.iter().copied().filter(|delay| *delay > 0).collect();
    if known.is_empty() {
        return DEFAULT_FPS;
    }
    known.sort_unstable();
    (1000.0 / known[known.len() / 2] as f32).round().clamp(1.0, 60.0)
}

/// The GIF's time per frame, when every frame has the same.
fn gif_delay(delays: &[u32]) -> Option<u32> {
    let first = *delays.first()?;
    (first > 0 && delays.iter().all(|delay| *delay == first)).then_some(first)
}

/// Opens the preview on `source` in the document whose id is `doc`, playing at the GIF's own
/// speed (`delays`, or the times in the frames' names). Frame layers show `crop` of the canvas,
/// or all of it. The grid and background of a preview it replaces carry over.
pub(crate) fn open(app: &mut PhotocraftApp, doc: u64, source: FrameSource, delays: Vec<u32>, crop: Option<Rect>) -> Result<Value, String> {
    let st = app.session.documents().iter().find(|st| st.doc.id.0 == doc).ok_or("The animation's canvas is not open")?;
    let delays = if delays.iter().any(|delay| *delay > 0) { delays } else { named_delays(&st.doc, &source) };
    let (grid, backdrop) = app.codex_runtime.animation.as_ref().map_or((Grid::Fit, Backdrop::Checker), |old| (old.grid, old.backdrop));
    let mut preview = Preview {
        doc,
        name: animation_name(&st.doc, &source),
        fps: fps_for(&delays),
        source,
        crop,
        delays,
        pending: Vec::new(),
        textures: Vec::new(),
        frame_size: [1, 1],
        area: Rect::EMPTY,
        canvas: Rect::EMPTY,
        rendered: 0,
        next_check: 0.0,
        error: None,
        current: 0,
        playing: true,
        clock: None,
        grid,
        backdrop,
        closed: false,
        scrolled: None,
        window: None,
    };
    preview.render(&st.doc);
    if let Some(error) = preview.error.clone() {
        return Err(error);
    }
    let frames = preview.count();
    app.codex_runtime.animation = Some(preview);
    Ok(json!({"open": true, "frames": frames}))
}

/// Opens the preview on an animation Codex just placed: `value` is `ai.apply`'s or `ai.open`'s
/// result, `target` the document `ai.apply` placed it in. A preview that can't be made leaves
/// the placed animation as it is.
pub(crate) fn open_result(app: &mut PhotocraftApp, value: &Value, target: Option<u64>) {
    let doc = match value.get("document").and_then(Value::as_u64) {
        Some(index) => usize::try_from(index).ok().and_then(|index| app.session.documents().get(index)).map(|st| st.doc.id.0),
        None => target,
    };
    let layers: Vec<LayerId> = value.get("layers").and_then(Value::as_array).map(|ids| ids.iter().filter_map(Value::as_u64).map(LayerId).collect()).unwrap_or_default();
    let source = match value.get("sheet").filter(|sheet| sheet.is_object()) {
        Some(sheet) => {
            let number = |key: &str| sheet.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(1);
            layers.first().map(|layer| FrameSource::Sheet { layer: *layer, columns: number("columns"), rows: number("rows") })
        }
        None => (layers.len() > 1).then(|| FrameSource::Layers(layers.clone())),
    };
    let delays = value
        .get("delays")
        .and_then(Value::as_array)
        .map(|delays| delays.iter().map(|delay| delay.as_u64().and_then(|ms| u32::try_from(ms).ok()).unwrap_or(0)).collect())
        .unwrap_or_default();
    // Frames placed in a selected area (`ai.apply`) show that area; a new canvas shows whole.
    let crop = match value.get("bounds").and_then(Value::as_array).map(Vec::as_slice) {
        Some([x, y, width, height]) if value.get("document").is_none() => {
            let int = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
            let size = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
            match (int(x), int(y), size(width), size(height)) {
                (Some(x), Some(y), Some(width), Some(height)) => Some(Rect::from_xywh(x, y, width, height)),
                _ => None,
            }
        }
        _ => None,
    };
    if let (Some(doc), Some(source)) = (doc, source) {
        let _ = open(app, doc, source, delays, crop);
    }
}

/// Layer › Flipbook › Preview Animation…, and the control API's `layer.flipbook.preview`:
/// `{layers?: [id], sheet?: {layer, columns?, rows?}, play?: bool}`. With neither `layers` nor
/// `sheet` it previews the animation the active document holds (`flipbook_cmds::find_animation`).
pub(crate) fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    (id == COMMAND).then(|| open_from(app, params))
}

fn open_from(app: &mut PhotocraftApp, params: &Value) -> Result<Value, String> {
    let st = app.session.active().ok_or("Open a canvas first")?;
    let doc = st.doc.id.0;
    let source = if let Some(layers) = params.get("layers") {
        let ids: Vec<LayerId> = layers.as_array().map(|ids| ids.iter().filter_map(Value::as_u64).map(LayerId).collect()).unwrap_or_default();
        if ids.len() < 2 {
            return Err("`layers` must list two or more layer ids".into());
        }
        FrameSource::Layers(ids)
    } else if let Some(sheet) = params.get("sheet") {
        let layer = sheet.get("layer").and_then(Value::as_u64).map(LayerId).ok_or("`sheet.layer` must be a layer id")?;
        let named = st.doc.layer(layer).and_then(|layer| flipbook_cmds::sheet_grid_in_name(&layer.name)).unwrap_or(flipbook_cmds::DEFAULT_SHEET_GRID);
        let number = |key: &str, default: usize| sheet.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(default);
        FrameSource::Sheet { layer, columns: number("columns", named.0), rows: number("rows", named.1) }
    } else {
        flipbook_cmds::find_animation(&st.doc, &st.selected_layers(), st.active_layer).ok_or_else(|| tl!("Select two or more frame layers or a flipbook sheet first.").to_owned())?
    };
    let result = open(app, doc, source, Vec::new(), None)?;
    if params.get("play").and_then(Value::as_bool) == Some(false)
        && let Some(preview) = app.codex_runtime.animation.as_mut()
    {
        preview.playing = false;
    }
    Ok(result)
}

/// The preview's state for `ui.inspect`, or null when it is closed.
pub(crate) fn inspect(app: &PhotocraftApp) -> Value {
    let Some(preview) = &app.codex_runtime.animation else { return Value::Null };
    let source = match &preview.source {
        FrameSource::Layers(ids) => json!({"layers": ids.iter().map(|id| id.0).collect::<Vec<_>>()}),
        FrameSource::Sheet { layer, columns, rows } => json!({"sheet": {"layer": layer.0, "columns": columns, "rows": rows}}),
    };
    json!({"name": preview.name, "frames": preview.count(), "current": preview.current, "playing": preview.playing, "fps": preview.fps,
        "grid": preview.grid.key(), "frameSize": preview.frame_size, "source": source, "closed": preview.closed, "error": preview.error})
}

/// The window, drawn every frame while a preview is open.
pub(crate) fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut preview) = app.codex_runtime.animation.take() else { return };
    let now = ctx.input(|input| input.time);
    if now >= preview.next_check {
        preview.next_check = now + RECHECK;
        sync(app, &mut preview);
    }
    preview.upload(ctx);
    let mut open = true;
    let mut action = None;
    let shown = egui::Window::new(tl!("Animation Preview"))
        .id(egui::Id::unique("yorusphoto-animation-preview"))
        .open(&mut open)
        // It first opens over the middle of the canvas.
        .default_pos(app.last_canvas_rect.center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_size(vec2(900.0, 680.0))
        .min_size(vec2(600.0, 480.0))
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| action = body(&mut preview, ui));
    preview.window = shown.map(|shown| shown.response.rect);
    preview.tick(ctx);
    if let Some(action) = action {
        run_action(app, &mut preview, action);
    }
    if open {
        app.codex_runtime.animation = Some(preview);
    }
}

/// Renders the frames again when their layers changed; notes a closed document.
fn sync(app: &PhotocraftApp, preview: &mut Preview) {
    match app.session.documents().iter().find(|st| st.doc.id.0 == preview.doc) {
        Some(st) => {
            preview.closed = false;
            if fingerprint(&st.doc, &preview.source) != preview.rendered {
                preview.render(&st.doc);
            }
        }
        None => preview.closed = true,
    }
}

fn run_action(app: &mut PhotocraftApp, preview: &mut Preview, action: Action) {
    let result = match (action, &preview.source) {
        (Action::Regrid, _) => {
            if let Some(st) = app.session.documents().iter().find(|st| st.doc.id.0 == preview.doc) {
                preview.render(&st.doc);
            }
            return;
        }
        (Action::MakeSheet, FrameSource::Layers(ids)) => {
            let params = json!({"layers": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "grid": preview.grid.key()});
            in_document(app, preview.doc, |app| app.run(flipbook_cmds::MAKE_SHEET, params))
        }
        (Action::Split, FrameSource::Sheet { columns, rows, .. }) => {
            let params = json!({"columns": columns, "rows": rows});
            in_document(app, preview.doc, |app| app.run(flipbook_cmds::SPLIT_SHEET, params))
        }
        _ => return,
    };
    if let Err(error) = result {
        preview.error = Some(error);
    }
}

/// Runs `f` with the document whose id is `doc` active. What it opens stays in front; after a
/// failure the document that was in front comes back.
fn in_document(app: &mut PhotocraftApp, doc: u64, f: impl FnOnce(&mut PhotocraftApp) -> Result<Value, String>) -> Result<Value, String> {
    let index = app.session.documents().iter().position(|st| st.doc.id.0 == doc).ok_or("The animation's canvas was closed")?;
    let previous = app.session.active_index();
    if !app.session.set_active(index) {
        return Err("Could not select the animation's canvas".into());
    }
    let result = f(app);
    if result.is_err()
        && let Some(previous) = previous
    {
        app.session.set_active(previous);
    }
    app.sync_views();
    result
}

// ---- drawing ----

fn body(preview: &mut Preview, ui: &mut egui::Ui) -> Option<Action> {
    let t = crate::ai_sidebar::palette(ui);
    header(preview, ui, &t);
    if let Some(error) = &preview.error {
        ui.colored_label(t.warning, error.as_str());
    }
    if preview.textures.is_empty() {
        return None;
    }
    let strip_height = 96.0;
    let size = ui.available_size();
    let main = (size.y - strip_height).max(260.0);
    let side = (size.x * 0.5 - 12.0).min(main - 128.0).max(160.0);
    let mut action = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(side);
            ui.label(RichText::new("GIF").size(16.0).strong().color(t.text));
            ui.add_space(2.0);
            player(preview, ui, &t, side);
            transport(preview, ui, &t);
        });
        ui.add_space(12.0);
        ui.vertical(|ui| action = sheet(preview, ui, &t, main));
    });
    ui.add_space(6.0);
    strip(preview, ui, &t);
    action
}

fn header(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&preview.name).size(18.0).strong().color(t.text));
        if let Some(ms) = gif_delay(&preview.delays) {
            ui.label(RichText::new(crate::i18n::fmt(tl!("GIF timing: {ms} ms per frame"), &[("ms", &ms.to_string())])).color(t.text_dim));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (backdrop, tip) in [(Backdrop::Light, tl!("Light")), (Backdrop::Dark, tl!("Dark")), (Backdrop::Checker, tl!("Checkerboard"))] {
                if swatch(ui, t, backdrop, preview.backdrop == backdrop).on_hover_text(tip).clicked() {
                    preview.backdrop = backdrop;
                }
            }
            ui.label(RichText::new(tl!("Background")).color(t.text_dim));
        });
    });
    ui.add_space(4.0);
}

/// Space plays and pauses and the arrow keys step while the pointer is over the window. The
/// shortcuts call it before their own keys, so the arrows never also nudge a layer behind it.
pub(crate) fn keys(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(preview) = app.codex_runtime.animation.as_mut() else { return };
    let over = preview.window.is_some_and(|window| ctx.pointer_hover_pos().is_some_and(|pos| window.contains(pos)));
    if !over || preview.textures.is_empty() {
        return;
    }
    let none = egui::Modifiers::NONE;
    let (space, left, right) = ctx.input_mut(|input| {
        (input.consume_key(none, egui::Key::Space), input.consume_key(none, egui::Key::ArrowLeft), input.consume_key(none, egui::Key::ArrowRight))
    });
    if space {
        preview.toggle();
    }
    if left {
        preview.step(-1);
    }
    if right {
        preview.step(1);
    }
}

/// The big player: the frame on show, its edges marked over the canvas's surround. A click plays
/// or pauses; under the pointer it shows which.
fn player(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens, side: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, t.radius, t.canvas);
    let frame = fit(rect.shrink(14.0), preview.frame_size);
    paint_backdrop(&painter, frame, preview.backdrop, 12.0);
    if let Some(texture) = preview.textures.get(preview.current) {
        painter.image(texture.id(), frame, uv(), Color32::WHITE);
    }
    painter.rect_stroke(frame, 0.0, Stroke::new(1.0, t.card_border), StrokeKind::Outside);
    painter.rect_stroke(rect, t.radius, Stroke::new(1.0, t.card_border), StrokeKind::Inside);
    badge(&painter, rect.min + vec2(10.0, 10.0), &format!("{} / {}", preview.current + 1, preview.textures.len()));
    if response.hovered() {
        painter.circle_filled(rect.center(), 30.0, Color32::from_black_alpha(170));
        if preview.playing {
            for dx in [-6.0, 6.0] {
                painter.rect_filled(Area::from_center_size(rect.center() + vec2(dx, 0.0), vec2(7.0, 24.0)), 1.0, Color32::WHITE);
            }
        } else {
            play_sign(&painter, rect.center() + vec2(3.0, 0.0), 12.0, Color32::WHITE);
        }
    }
    let tip = if preview.playing { tl!("Pause") } else { tl!("Play") };
    if response.on_hover_text(tip).clicked() {
        preview.toggle();
    }
}

fn transport(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if icon_button(ui, t, Icon::Previous, 32.0, false).on_hover_text(tl!("Previous frame")).clicked() {
            preview.step(-1);
        }
        let (icon, tip) = if preview.playing { (Icon::Pause, tl!("Pause")) } else { (Icon::Play, tl!("Play")) };
        if icon_button(ui, t, icon, 40.0, true).on_hover_text(tip).clicked() {
            preview.toggle();
        }
        if icon_button(ui, t, Icon::Next, 32.0, false).on_hover_text(tl!("Next frame")).clicked() {
            preview.step(1);
        }
        ui.add_space(6.0);
        let number = (preview.current + 1).to_string();
        let count = preview.textures.len().to_string();
        let label = crate::i18n::fmt(tl!("Frame {number} of {count}"), &[("number", &number), ("count", &count)]);
        ui.label(RichText::new(label).strong().color(t.text));
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Speed")).color(t.text_dim));
        ui.add(egui::Slider::new(&mut preview.fps, 1.0..=60.0).step_by(1.0).suffix(" fps"));
    });
    ui.label(RichText::new(tl!("Space plays and pauses · ← → step a frame")).small().color(t.text_faint));
}

/// The flipbook sheet: the frames in their grid (the grid Make Flipbook Sheet uses, or a
/// sheet's own), the frame on show lit. A click shows a frame.
fn sheet(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens, height: f32) -> Option<Action> {
    let mut action = None;
    ui.label(RichText::new(tl!("Flipbook")).size(16.0).strong().color(t.text));
    ui.add_space(2.0);
    let count = preview.textures.len();
    let (columns, rows) = match &mut preview.source {
        FrameSource::Layers(_) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Grid")).color(t.text_dim));
                ui.selectable_value(&mut preview.grid, Grid::Fit, tl!("Fit"));
                ui.selectable_value(&mut preview.grid, Grid::Square, tl!("Square"));
                ui.add_enabled_ui(count <= 64, |ui| ui.selectable_value(&mut preview.grid, Grid::Roblox, "Roblox"))
                    .inner
                    .on_disabled_hover_text(tl!("Roblox flipbooks hold up to 64 frames (8 × 8)."));
            });
            flipbook_cmds::sheet_layout(count, preview.grid.key()).unwrap_or((count.max(1), 1))
        }
        FrameSource::Sheet { columns, rows, .. } => {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Columns")).color(t.text_dim));
                changed |= ui.add(egui::DragValue::new(columns).range(1..=16)).changed();
                ui.label(RichText::new(tl!("Rows")).color(t.text_dim));
                changed |= ui.add(egui::DragValue::new(rows).range(1..=16)).changed();
            });
            if changed {
                action = Some(Action::Regrid);
            }
            (*columns, *rows)
        }
    };
    let layers = matches!(preview.source, FrameSource::Layers(_));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), (height - 132.0).max(150.0)), Sense::hover());
    sheet_cells(preview, ui, t, rect, (columns, rows), layers);
    let (width, height) = if layers {
        (preview.canvas.width().saturating_mul(columns as u32), preview.canvas.height().saturating_mul(rows as u32))
    } else {
        (preview.canvas.width(), preview.canvas.height())
    };
    ui.add_space(6.0);
    let info = crate::i18n::fmt(
        tl!("{columns} × {rows} sheet · {width} × {height} px"),
        &[("columns", &columns.to_string()), ("rows", &rows.to_string()), ("width", &width.to_string()), ("height", &height.to_string())],
    );
    ui.label(RichText::new(info).color(t.text_dim));
    ui.add_space(4.0);
    let label = if layers { tl!("Make Flipbook Sheet") } else { tl!("Split Flipbook Sheet") };
    let button = egui::Button::new(RichText::new(label).strong().color(t.primary_text)).fill(t.primary_bg).min_size(vec2(180.0, 32.0)).corner_radius(t.radius);
    if ui.add_enabled(!preview.closed, button).clicked() {
        action = Some(if layers { Action::MakeSheet } else { Action::Split });
    }
    action
}

/// The sheet's cells in `rect`. Made from frame layers, each cell is the whole canvas (as Make
/// Flipbook Sheet makes them) with the frame where its pixels are; a sheet's cells are its own.
fn sheet_cells(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens, rect: Area, (columns, rows): (usize, usize), layers: bool) {
    let cell_aspect = aspect(if layers { preview.canvas } else { preview.area });
    let (across, down) = (columns.max(1) as f32, rows.max(1) as f32);
    let cell_width = (rect.width() / across).min(rect.height() / down * cell_aspect).max(4.0);
    let cell_height = cell_width / cell_aspect;
    let origin = rect.center() - vec2(cell_width * across, cell_height * down) / 2.0;
    let painter = ui.painter_at(rect);
    for index in 0..columns * rows {
        let place = vec2((index % columns) as f32 * cell_width, (index / columns) as f32 * cell_height);
        let cell = Area::from_min_size(origin + place, vec2(cell_width, cell_height)).shrink(2.0);
        let Some(texture) = preview.textures.get(index) else {
            // An empty cell of the sheet.
            painter.rect_stroke(cell, 3.0, Stroke::new(1.0, t.text_faint), StrokeKind::Inside);
            continue;
        };
        let response = ui.interact(cell, ui.scope_id().with(("animation-preview-cell", index)), Sense::click());
        paint_backdrop(&painter, cell, preview.backdrop, (cell_width / 8.0).clamp(3.0, 10.0));
        let shown = if layers { within(cell, preview.area, preview.canvas) } else { cell };
        painter.image(texture.id(), shown, uv(), Color32::WHITE);
        let lit = index == preview.current;
        let stroke = if lit {
            Stroke::new(3.0, t.accent)
        } else if response.hovered() {
            Stroke::new(1.5, t.text_dim)
        } else {
            Stroke::new(1.0, t.card_border)
        };
        painter.rect_stroke(cell, 3.0, stroke, if lit { StrokeKind::Outside } else { StrokeKind::Inside });
        if cell_width >= 34.0 {
            badge(&painter, cell.min + vec2(4.0, 4.0), &(index + 1).to_string());
        }
        if response.on_hover_text(tl!("Click a frame to show it.")).clicked() {
            preview.select(index);
        }
    }
}

/// Every frame in a row, the one on show lit and kept in view. A click shows a frame.
fn strip(preview: &mut Preview, ui: &mut egui::Ui, t: &Tokens) {
    egui::ScrollArea::horizontal().id_salt("animation-preview-strip").auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal(|ui| {
            for index in 0..preview.textures.len() {
                let (rect, response) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::click());
                let painter = ui.painter_at(rect.expand(3.0));
                painter.rect_filled(rect, 4.0, t.canvas);
                let frame = fit(rect.shrink(6.0), preview.frame_size);
                paint_backdrop(&painter, frame, preview.backdrop, 6.0);
                painter.image(preview.textures[index].id(), frame, uv(), Color32::WHITE);
                let lit = index == preview.current;
                let stroke = if lit {
                    Stroke::new(3.0, t.accent)
                } else if response.hovered() {
                    Stroke::new(1.5, t.text_dim)
                } else {
                    Stroke::new(1.0, t.card_border)
                };
                painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
                badge(&painter, rect.min + vec2(4.0, 4.0), &(index + 1).to_string());
                if lit && preview.scrolled != Some(index) {
                    response.scroll_to_me(Some(egui::Align::Center));
                    preview.scrolled = Some(index);
                }
                if response.clicked() {
                    preview.select(index);
                }
            }
        });
    });
}

fn swatch(ui: &mut egui::Ui, t: &Tokens, backdrop: Backdrop, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    paint_backdrop(&ui.painter_at(rect), rect, backdrop, 5.5);
    let stroke = if on {
        Stroke::new(2.0, t.accent)
    } else if response.hovered() {
        Stroke::new(1.0, t.text_dim)
    } else {
        Stroke::new(1.0, t.card_border)
    };
    ui.painter().rect_stroke(rect, 4.0, stroke, StrokeKind::Outside);
    response
}

fn paint_backdrop(painter: &egui::Painter, rect: Area, backdrop: Backdrop, square: f32) {
    match backdrop {
        Backdrop::Dark => {
            painter.rect_filled(rect, 0.0, Color32::from_gray(28));
        }
        Backdrop::Light => {
            painter.rect_filled(rect, 0.0, Color32::from_gray(246));
        }
        Backdrop::Checker => {
            // White and light grey squares, as transparent pixels are shown on the canvas.
            painter.rect_filled(rect, 0.0, Color32::WHITE);
            let (across, down) = ((rect.width() / square).ceil() as usize, (rect.height() / square).ceil() as usize);
            for row in 0..down {
                for column in (row % 2..across).step_by(2) {
                    let min = rect.min + vec2(column as f32 * square, row as f32 * square);
                    painter.rect_filled(Area::from_min_size(min, vec2(square, square)).intersect(rect), 0.0, Color32::from_gray(204));
                }
            }
        }
    }
}

/// A small dark pill with `text`, for frame numbers.
fn badge(painter: &egui::Painter, at: egui::Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.5), Color32::WHITE);
    let rect = Area::from_min_size(at, galley.size() + vec2(10.0, 4.0));
    painter.rect_filled(rect, rect.height() / 2.0, Color32::from_black_alpha(170));
    painter.galley(rect.min + vec2(5.0, 2.0), galley, Color32::WHITE);
}

/// A round transport button with its icon drawn; `primary` fills it with the accent.
fn icon_button(ui: &mut egui::Ui, t: &Tokens, icon: Icon, size: f32, primary: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let painter = ui.painter();
    let (fill, ink) = if primary {
        (t.primary_bg, t.primary_text)
    } else if response.hovered() {
        (t.hover, t.text)
    } else {
        (t.field, t.text)
    };
    painter.rect_filled(rect, size / 2.0, fill);
    if !primary {
        painter.rect_stroke(rect, size / 2.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    let (centre, s) = (rect.center(), size * 0.2);
    match icon {
        Icon::Play => play_sign(painter, centre + vec2(s * 0.15, 0.0), s, ink),
        Icon::Pause => {
            for dx in [-0.55, 0.55] {
                painter.rect_filled(Area::from_center_size(centre + vec2(dx * s, 0.0), vec2(s * 0.55, s * 1.8)), 1.0, ink);
            }
        }
        Icon::Previous | Icon::Next => {
            let way = if matches!(icon, Icon::Next) { 1.0 } else { -1.0 };
            let (tip, back) = (centre + vec2(way * s * 0.55, 0.0), centre + vec2(-way * s * 0.6, 0.0));
            painter.add(polygon(vec![back + vec2(0.0, -s * 0.85), back + vec2(0.0, s * 0.85), tip], ink));
            painter.rect_filled(Area::from_center_size(centre + vec2(way * s * 0.85, 0.0), vec2(s * 0.35, s * 1.7)), 1.0, ink);
        }
    }
    response
}

fn play_sign(painter: &egui::Painter, centre: egui::Pos2, size: f32, colour: Color32) {
    painter.add(polygon(vec![centre + vec2(-size * 0.7, -size), centre + vec2(-size * 0.7, size), centre + vec2(size, 0.0)], colour));
}

/// A filled convex polygon, its points put clockwise on screen as egui's anti-aliasing wants.
fn polygon(mut points: Vec<egui::Pos2>, colour: Color32) -> egui::Shape {
    let twice_area: f32 = points.iter().zip(points.iter().cycle().skip(1)).map(|(a, b)| a.x * b.y - b.x * a.y).sum();
    if twice_area < 0.0 {
        points.reverse();
    }
    egui::Shape::convex_polygon(points, colour, Stroke::NONE)
}

fn aspect(rect: Rect) -> f32 {
    rect.width().max(1) as f32 / rect.height().max(1) as f32
}

/// The largest rectangle of `size`'s shape inside `space`, centred.
fn fit(space: Area, size: [u32; 2]) -> Area {
    let (width, height) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (space.width() / width).min(space.height() / height);
    Area::from_center_size(space.center(), vec2(width * scale, height * scale))
}

/// Where `area` of `canvas` falls in `cell`, the canvas filling the cell.
fn within(cell: Area, area: Rect, canvas: Rect) -> Area {
    let x = |value: i32| cell.left() + (value - canvas.x0) as f32 / canvas.width().max(1) as f32 * cell.width();
    let y = |value: i32| cell.top() + (value - canvas.y0) as f32 / canvas.height().max(1) as f32 * cell.height();
    Area::from_min_max(egui::pos2(x(area.x0), y(area.y0)), egui::pos2(x(area.x1), y(area.y1)))
}

fn uv() -> Area {
    Area::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// A `size` x `size` PNG drawn as a 2 x 2 grid: red, green, blue and yellow cells.
    fn grid_png(size: u32) -> String {
        let half = size / 2;
        let mut pixels = Vec::new();
        for y in 0..size {
            for x in 0..size {
                pixels.extend(match (x < half, y < half) {
                    (true, true) => [255, 0, 0, 255],
                    (false, true) => [0, 255, 0, 255],
                    (true, false) => [0, 0, 255, 255],
                    (false, false) => [255, 255, 0, 255],
                });
            }
        }
        let image = photocraft_codecs::Image::from_u8(size, size, photocraft_codecs::ChannelLayout::Rgba, pixels).unwrap();
        base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
    }

    /// One frame of the window.
    fn frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
        ctx.begin_pass(egui::RawInput::default());
        show(app, ctx);
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn playback_moves_on_a_frame_a_step_and_wraps_round() {
        assert_eq!(next_frame(0, 4, 10.0, 10.05, 0.1), (0, 10.0), "under a step: no change");
        assert_eq!(next_frame(0, 4, 10.0, 10.25, 0.1), (2, 10.2));
        assert_eq!(next_frame(3, 4, 10.0, 10.1, 0.1).0, 0, "the last frame wraps to the first, a step after (0.1 + 10.0 isn't exactly 10.1)");
        assert_eq!(next_frame(1, 4, 10.0, 10.0999999, 0.1).0, 2, "a repaint a hair early still moves on");
        assert_eq!(fps_for(&[100, 100, 100]), 10.0);
        assert_eq!(fps_for(&[0, 0]), DEFAULT_FPS);
        assert_eq!(fps_for(&[40, 50, 60]), 20.0, "the median's rate");
        assert_eq!(gif_delay(&[80, 80]), Some(80));
        assert_eq!(gif_delay(&[80, 90]), None);
    }

    #[test]
    fn frames_preview_with_their_sheet_and_make_it_and_the_sheet_previews_and_splits() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        // Four frame layers, as a GIF drawn as a 2 x 2 grid arrives.
        app.run("ai.open", json!({"data": grid_png(24), "name": "Torch", "grid": {"columns": 2, "rows": 2}})).unwrap();
        let opened = menu(&mut app, COMMAND, &json!({})).expect("the preview's command").unwrap();
        assert_eq!(opened["frames"], 4);
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        let state = inspect(&app);
        assert_eq!((state["name"].as_str(), state["frames"].as_u64(), state["playing"].as_bool()), (Some("Torch"), Some(4), Some(true)));
        assert_eq!(state["frameSize"], json!([12, 12]));
        let preview = app.codex_runtime.animation.as_mut().unwrap();
        assert_eq!(preview.textures.len(), 4, "every frame uploaded");
        preview.step(1);
        assert_eq!((preview.current, preview.playing), (1, false), "stepping pauses");
        preview.step(-2);
        assert_eq!(preview.current, 3, "stepping back wraps round");
        preview.grid = Grid::Roblox;
        // Make Flipbook Sheet from the preview opens the sheet in a new canvas.
        let mut preview = app.codex_runtime.animation.take().unwrap();
        let documents = app.session.documents().len();
        run_action(&mut app, &mut preview, Action::MakeSheet);
        assert_eq!(preview.error, None);
        app.codex_runtime.animation = Some(preview);
        assert_eq!(app.session.documents().len(), documents + 1);
        let sheet = app.session.active().unwrap();
        assert_eq!((sheet.doc.size.width, sheet.doc.size.height), (24, 24), "a 2 x 2 sheet of 12 px frames");
        // The sheet previews from its own name's grid, and splits back into four frames.
        let opened = menu(&mut app, COMMAND, &json!({"play": false})).unwrap().unwrap();
        assert_eq!(opened["frames"], 4);
        let state = inspect(&app);
        assert_eq!(state["source"]["sheet"]["columns"], 2);
        assert_eq!(state["playing"], false);
        assert_eq!(state["grid"], "roblox", "the grid carries over to the next preview");
        let mut preview = app.codex_runtime.animation.take().unwrap();
        run_action(&mut app, &mut preview, Action::Split);
        assert_eq!(preview.error, None);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 4);
    }

    #[test]
    fn codex_results_open_the_preview_on_their_frames() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let opened = app.run("ai.open", json!({"data": grid_png(24), "name": "Torch", "grid": {"columns": 2, "rows": 2}})).unwrap();
        open_result(&mut app, &opened, None);
        assert_eq!(inspect(&app)["frameSize"], json!([12, 12]), "a new canvas shows whole");
        let doc = app.session.active().unwrap().doc.id.0;
        let placed = json!({"layers": opened["layers"], "frames": 4, "delays": [100, 100, 100, 100], "bounds": [2, 2, 6, 6]});
        open_result(&mut app, &placed, Some(doc));
        let state = inspect(&app);
        assert_eq!(state["frameSize"], json!([6, 6]), "frames placed in a selected area show that area");
        assert_eq!(state["fps"], 10.0, "at the GIF's own speed");
        assert_eq!(state["playing"], true);
    }

    #[test]
    fn nothing_to_preview_says_what_to_select() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        assert!(menu(&mut app, COMMAND, &json!({})).unwrap().is_err(), "no canvas");
        assert!(menu(&mut app, "layer.other", &json!({})).is_none());
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        let layers = json!({"layers": [1]});
        assert!(menu(&mut app, COMMAND, &layers).unwrap().is_err(), "one layer is no animation");
        assert_eq!(inspect(&app), Value::Null);
    }
}
