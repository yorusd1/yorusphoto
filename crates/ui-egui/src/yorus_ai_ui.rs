//! Native YorusAi Direct controls and selection-safe provider results.
//! Provider networking and credentials belong to the desktop service. Keys and image bytes
//! are runtime-only; the saved UI state contains ordinary operation preferences.

use crate::PhotocraftApp;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use photocraft_codecs::{ChannelLayout, DecodeOptions, EncodeOptions, Format, Image, Limits, SampleType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};

const MAX_REFS: usize = 13;
const MAX_REFERENCE_BYTES: usize = 64 << 20;
const MODELS: &[(&str, &str)] = &[
    ("nano-banana-2.1", "Nano Banana 2.1"),
    ("ideogram-4-5-precise-edit", "Ideogram 4.5 Precise Edit"),
    ("flux-3", "FLUX 3"),
    ("seedream-5.5", "Seedream 5.5"),
];
/// Saved choices from earlier builds and what they become: Seedream 5 Pro was replaced by
/// Seedream 5.5; GPT Image through an OpenAI API key was removed (Codex keeps its own image
/// generation through the ChatGPT account).
const RETIRED_MODELS: &[(&str, &str)] = &[("seedream-5-pro", "seedream-5.5"), ("gpt-image-2", "nano-banana-2.1")];
/// Seedream 5.5 backgrounds: opaque keeps an edit's pixels solid; transparent asks for a PNG
/// with alpha (sprites, icons, VFX); auto lets the model decide.
const BACKGROUNDS: &[&str] = &["opaque", "transparent", "auto"];
/// Seedream 5.5 Layer Separation: the most images one request returns, base included.
pub const MAX_LAYER_IMAGES: u32 = 100;
const RATIOS: &[&str] = &["auto", "1:1", "16:9", "21:9", "3:2", "4:3", "5:4", "4:5", "3:4", "9:16", "2:3"];

pub struct DirectRequest {
    pub operation: String,
    pub model: String,
    pub prompt: String,
    pub ratio: String,
    pub resolution: String,
    pub quality: String,
    pub scale_factor: u32,
    pub reference: Option<Vec<u8>>,
    pub mask: Option<Vec<u8>>,
    pub references: Vec<Vec<u8>>,
    pub input_size: [u32; 2],
    /// Seedream 5.5 generation: `opaque`, `transparent` or `auto`.
    pub background: String,
    /// Seedream 5.5 Layer Separation: images to return at most (2..=100, base included).
    pub max_layers: u32,
    /// Seedream 5.5 Layer Separation: read the text on text layers (names them by it).
    pub text_layers: bool,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirectBounds {
    pub normalized: Option<[f64; 4]>,
    pub absolute: Option<[f64; 4]>,
    pub package_size: Option<[u32; 2]>,
}

pub struct DirectImage {
    pub bytes: Vec<u8>,
    pub name: String,
    pub z_index: i32,
    /// Informational only. The provider service expands decomposition parts into full
    /// input-size transparent frames, keeping the captured rectangle and mask unchanged.
    pub bounds: Option<DirectBounds>,
}

pub enum DirectEvent {
    Status(String),
    Completed(Vec<DirectImage>),
    Error(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct DirectKeys {
    pub replicate: bool,
    pub byteplus: bool,
}

pub type DirectGenerateFn = Box<dyn Fn(DirectRequest, Sender<DirectEvent>)>;
pub type DirectKeyStatusFn = Box<dyn Fn(Sender<Result<DirectKeys, String>>)>;
pub type DirectSaveKeyFn = Box<dyn Fn(String, String, Sender<Result<DirectKeys, String>>)>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub open: bool,
    pub tab: String,
    pub model: String,
    pub prompt: String,
    pub ratio: String,
    pub resolution: String,
    pub quality: String,
    pub ideogram_quality: String,
    pub scale_factor: u32,
    pub layer_prompt: String,
    pub layer_resolution: String,
    /// Seedream 5.5 background for generation (`BACKGROUNDS`).
    pub background: String,
    /// Layer Separation: most images per request, base included.
    pub layer_max: u32,
    /// Layer Separation: read text layers' text.
    pub layer_text: bool,
    /// A result with more pixels than the selection enlarges the document instead of being
    /// scaled down (`ai.apply` `"fit":"grow"`); the layer keeps its place and size.
    pub grow_document: bool,
    pub new_canvas: bool,
    pub upscale_new_canvas: bool,
    pub width: u32,
    pub height: u32,
    pub status: String,
    pub error: bool,
    pub builder: PromptBuilder,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            tab: "remix".into(),
            model: "nano-banana-2.1".into(),
            prompt: String::new(),
            ratio: "auto".into(),
            resolution: "1K".into(),
            quality: "high".into(),
            ideogram_quality: "medium".into(),
            scale_factor: 2,
            layer_prompt: String::new(),
            layer_resolution: "1K".into(),
            background: "opaque".into(),
            layer_max: 17,
            layer_text: false,
            grow_document: true,
            new_canvas: false,
            upscale_new_canvas: false,
            width: 1024,
            height: 1024,
            status: String::new(),
            error: false,
            builder: PromptBuilder::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptBuilder {
    pub input: String,
    pub project: String,
    pub output: String,
    pub lighting: String,
    pub style: String,
    pub materials: String,
    pub atmosphere: String,
    pub style_reference: bool,
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self {
            input: "3D viewport".into(),
            project: "architectural project".into(),
            output: "photorealistic architectural visualization".into(),
            lighting: "soft daylight".into(),
            style: "contemporary".into(),
            materials: "preserve the original materials".into(),
            atmosphere: "natural and inviting".into(),
            style_reference: false,
        }
    }
}

struct Reference {
    name: String,
    bytes: Vec<u8>,
    size: [u32; 2],
    thumbnail: Option<egui::TextureHandle>,
}

struct Placement {
    target: Option<Value>,
    size: [u32; 2],
    operation: String,
}

type CapturedInput = (Option<Vec<u8>>, Option<Vec<u8>>, Placement);

#[derive(Default)]
pub struct Runtime {
    events: Option<Receiver<DirectEvent>>,
    key_reply: Option<Receiver<Result<DirectKeys, String>>>,
    cancel: Option<Arc<AtomicBool>>,
    placement: Option<Placement>,
    pending: VecDeque<DirectImage>,
    references: Vec<Reference>,
    keys: DirectKeys,
    keys_loaded: bool,
    show_keys: bool,
    key_provider: String,
    key_input: String,
}

impl Runtime {
    pub fn busy(&self) -> bool {
        self.events.is_some()
    }
    pub fn inspect(&self) -> Value {
        json!({"busy":self.busy(),"keys":self.keys,"keysLoaded":self.keys_loaded,
            "referenceCount":self.references.len(),"retainedResults":self.pending.len()})
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn disabled_reason(app: &PhotocraftApp, operation: &str) -> Option<String> {
    if app.services.yorus_ai_generate.is_none() {
        return Some("YorusAi Direct requires the native desktop build".into());
    }
    if app.yorus_ai_runtime.busy() || app.codex_runtime.busy() {
        return Some("another AI operation is running; wait or stop it first".into());
    }
    if !app.yorus_ai_runtime.pending.is_empty() {
        return Some("place or discard retained provider results first".into());
    }
    let needs_source = operation != "remix" || !app.ui.yorus_ai.new_canvas;
    if needs_source && app.session.active().is_none() {
        return Some("no document open; open an image or use remix mode canvas".into());
    }
    if needs_source
        && operation != "layers"
        && !app.session.active().is_some_and(|d| d.doc.selection.as_ref().is_some_and(|s| !s.content_bounds().intersect(&d.doc.bounds()).is_empty()))
    {
        return Some("no selection; select a nonempty area first".into());
    }
    let route = match operation {
        "upscale" => "replicate",
        "layers" => "byteplus",
        _ => provider(&app.ui.yorus_ai.model, app.yorus_ai_runtime.keys),
    };
    if !has_key(app.yorus_ai_runtime.keys, route) {
        return Some(format!("configure a {} API key in the YorusAi panel first", provider_name(route)));
    }
    None
}

/// Native shell commands merged with the engine registry for live MCP and embedded agents.
/// Enablement describes the current panel defaults; parameters may choose another model or
/// a fresh canvas. Credentials are deliberately absent from both metadata and parameters.
pub fn command_specs(app: &PhotocraftApp) -> Vec<Value> {
    let remix = r#"{"model":"nano-banana-2.1"|"ideogram-4-5-precise-edit"|"flux-3"|"seedream-5.5"?,"prompt":string?,"mode":"selection"|"canvas"?,"ratio":"auto"|"1:1"|"16:9"|"21:9"|"3:2"|"4:3"|"5:4"|"4:5"|"3:4"|"9:16"|"2:3"? (model-dependent),"resolution":string? (Nano:1K|2K|4K; Seedream:1K|1.5K|2K; FLUX:768sq|1k|1.5k|2k|4k),"quality":"low"|"medium"|"high"|"very_low"? (Ideogram),"background":"opaque"|"transparent"|"auto"? (Seedream 5.5 on BytePlus: transparent returns a PNG with alpha),"width":1..16384?,"height":1..16384?} Uses current prompt/options and additional reference images from the panel when omitted. Seedream 5.5 runs on BytePlus ModelArk (with only a Replicate key it falls back to Seedream 5.0 Pro, which Replicate hosts). Seedream prompts <=4000 characters; others <=64KiB. Selected-area mode captures visible merged pixels, original geometry and exact coverage before starting, then adds a separate layer there. Canvas mode uses only the prompt/additional references and opens an independent canvas at width/height (<=16777216 pixels). Ideogram requires selection mode and sends the selection's black-edit/white-preserve mask. Provider key must already be configured; requests bill that API account. Returns immediately; poll ui.inspect.yorusAi.busy and ui.inspect.yorusAiSettings.status and use yorusai.cancel to stop."#;
    let upscale = r#"{"scaleFactor":2..8?,"mode":"selection"|"canvas"?} Crystal Upscale via Replicate. Requires a nonempty selection and saved Replicate key. Captures the original visible merged rectangle and selection coverage. Selection mode adds a separate layer at the same position/size; explicit canvas mode opens a fresh canvas at captured width*factor by height*factor. Output must fit <=16777216 pixels. Returns immediately; poll ui.inspect.yorusAi.busy and ui.inspect.yorusAiSettings.status. Provider bills the configured API account."#;
    let layers = r#"{"prompt":string? (optional instructions, <=4000 characters),"resolution":"1K"|"1.5K"|"2K"?,"maxLayers":2..100? (images returned at most, base included; each is billed),"textLayers":bool? (read text layers' text and name them by it)} Layer Separation with Seedream 5.5 via BytePlus ModelArk. Requires an open image and configured BytePlus key. Uses the selected area and original selection coverage when present, otherwise the whole visible canvas. Downloads positioned transparent artifacts, preserves their z-order, and adds separate layers inside that saved original frame. Does not change source layers or a newer selection; closed/resized source retains results for explicit recovery. Returns immediately; poll ui.inspect.yorusAi.busy and ui.inspect.yorusAiSettings.status. Provider bills the configured API account."#;
    let run = format!(
        "{{\"operation\":\"remix\"|\"upscale\"|\"layers\"?, ...operation parameters}} Defaults to the visible YorusAi workflow. Remix: {remix} Upscale: {upscale} Layers: {layers}"
    );
    let current = if ["remix", "upscale", "layers"].contains(&app.ui.yorus_ai.tab.as_str()) { app.ui.yorus_ai.tab.as_str() } else { "remix" };
    [
        ("yorusai.run", "Run YorusAi operation", Some(current), run),
        ("yorusai.remix", "Generate YorusAi remix", Some("remix"), remix.into()),
        ("yorusai.upscale", "Upscale with YorusAi", Some("upscale"), upscale.into()),
        ("yorusai.layers", "Separate layers with YorusAi", Some("layers"), layers.into()),
        ("yorusai.inspect", "Inspect YorusAi settings", None, "{} -> {settings,runtime:{busy,keys:{replicate,byteplus},keysLoaded,referenceCount,retainedResults}}. Only key presence is returned; key values and image bytes are never exposed.".into()),
        ("yorusai.cancel", "Stop YorusAi operation", None, "{} -> {stopping:bool}. Requests cancellation of the current Direct provider operation; late image results are not placed. Idempotent when idle.".into()),
    ].into_iter().map(|(id, label, operation, params)| {
        let reason = operation.and_then(|operation| disabled_reason(app, operation));
        let needs_document = app.session.active().is_none() && reason.as_deref().is_some_and(|r| r.starts_with("no document"));
        json!({"id":id,"label":label,"menu":[],"shortcut":null,"params":params,"enabled":reason.is_none(),"disabledReason":reason,"needsDocument":needs_document})
    }).collect()
}

fn status(app: &mut PhotocraftApp, message: impl Into<String>, error: bool) {
    app.ui.yorus_ai.status = message.into();
    app.ui.yorus_ai.error = error;
}

fn provider(model: &str, keys: DirectKeys) -> &'static str {
    match model {
        // Seedream 5.5 is on BytePlus ModelArk only; Replicate still hosts Seedream 5.0 Pro.
        "seedream-5.5" if keys.byteplus || !keys.replicate => "byteplus",
        _ => "replicate",
    }
}

fn has_key(keys: DirectKeys, name: &str) -> bool {
    match name {
        "replicate" => keys.replicate,
        "byteplus" => keys.byteplus,
        _ => false,
    }
}

fn provider_name(name: &str) -> &'static str {
    match name {
        "replicate" => "Replicate",
        "byteplus" => "BytePlus ModelArk",
        _ => "Provider",
    }
}

fn resolutions(model: &str) -> &'static [&'static str] {
    match model {
        "flux-3" => &["768sq", "1k", "1.5k", "2k", "4k"],
        "seedream-5.5" => &["1K", "1.5K", "2K"],
        _ => &["1K", "2K", "4K"],
    }
}

fn ratios(model: &str) -> Vec<&'static str> {
    // Seedream takes any ratio as explicit pixels on BytePlus, but its Replicate fallback lists
    // fixed ratios without 5:4 and 4:5.
    RATIOS.iter().copied().filter(|r| model != "seedream-5.5" || !matches!(*r, "5:4" | "4:5")).collect()
}

fn normalize(s: &mut State) {
    if let Some((_, replacement)) = RETIRED_MODELS.iter().find(|(old, _)| *old == s.model) {
        s.model = (*replacement).into();
    }
    if !MODELS.iter().any(|(model, _)| *model == s.model) {
        s.model = "nano-banana-2.1".into();
    }
    if !["remix", "upscale", "layers"].contains(&s.tab.as_str()) {
        s.tab = "remix".into();
    }
    if !resolutions(&s.model).contains(&s.resolution.as_str()) {
        let changed_case = if s.model == "flux-3" { s.resolution.to_lowercase() } else { s.resolution.to_uppercase() };
        s.resolution =
            if resolutions(&s.model).contains(&changed_case.as_str()) { changed_case } else { resolutions(&s.model).first().copied().unwrap_or("1K").into() };
    }
    if !ratios(&s.model).contains(&s.ratio.as_str()) {
        s.ratio = "auto".into();
    }
    if !["low", "medium", "high"].contains(&s.quality.as_str()) {
        s.quality = "high".into();
    }
    if !["very_low", "low", "medium", "high"].contains(&s.ideogram_quality.as_str()) {
        s.ideogram_quality = "medium".into();
    }
    if !["1K", "1.5K", "2K"].contains(&s.layer_resolution.as_str()) {
        s.layer_resolution = "1K".into();
    }
    if !BACKGROUNDS.contains(&s.background.as_str()) {
        s.background = "opaque".into();
    }
    s.layer_max = s.layer_max.clamp(2, MAX_LAYER_IMAGES);
    s.scale_factor = s.scale_factor.clamp(2, 8);
    if s.model == "ideogram-4-5-precise-edit" {
        s.new_canvas = false;
    }
}

fn check_size([width, height]: [u32; 2]) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > photocraft_engine::generative_cmds::MAX_SIDE
        || height > photocraft_engine::generative_cmds::MAX_SIDE
        || u64::from(width) * u64::from(height) > photocraft_engine::generative_cmds::MAX_PIXELS
    {
        return Err("The image must fit within 16,777,216 pixels and 16,384 pixels per side.".into());
    }
    Ok(())
}

fn capture(app: &mut PhotocraftApp, operation: &str) -> Result<CapturedInput, String> {
    let s = &app.ui.yorus_ai;
    if operation == "remix" && s.new_canvas {
        let size = [s.width, s.height];
        check_size(size)?;
        return Ok((None, None, Placement { target: None, size, operation: operation.into() }));
    }
    let active = app.session.active().ok_or("Open an image first.")?;
    if operation != "layers" && active.doc.selection.is_none() {
        return Err("Select a nonempty area first.".into());
    }
    let value = app.run("ai.capture", json!({}))?;
    let target = value.get("target").cloned().ok_or("Image capture returned no destination.")?;
    let width = value.get("width").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or("Image capture returned no width.")?;
    let height = value.get("height").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or("Image capture returned no height.")?;
    let reference =
        STANDARD.decode(value.get("reference").and_then(Value::as_str).ok_or("Image capture returned no reference.")?).map_err(|e| e.to_string())?;
    let mask = if operation == "remix" && app.ui.yorus_ai.model == "ideogram-4-5-precise-edit" { edit_mask(&target, [width, height])? } else { None };
    let new_upscale = operation == "upscale" && app.ui.yorus_ai.upscale_new_canvas;
    let size = if new_upscale {
        let factor = app.ui.yorus_ai.scale_factor;
        [width.checked_mul(factor).ok_or("Upscale width is too large.")?, height.checked_mul(factor).ok_or("Upscale height is too large.")?]
    } else {
        [width, height]
    };
    check_size(size)?;
    Ok((Some(reference), mask, Placement { target: if new_upscale { None } else { Some(target) }, size, operation: operation.into() }))
}

fn decode_image(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() > MAX_REFERENCE_BYTES {
        return Err("Reference image exceeds 64 MiB.".into());
    }
    let format = photocraft_codecs::detect(bytes).ok_or("Use a PNG, JPEG or WebP reference image.")?;
    if !matches!(format, Format::Png | Format::Jpeg | Format::WebP) {
        return Err("Use a PNG, JPEG or WebP reference image.".into());
    }
    photocraft_codecs::decode_with(
        bytes,
        &DecodeOptions { limits: Limits { max_width: 16_384, max_height: 16_384, max_pixels: 16_777_216, max_alloc: 128 << 20 }, ..Default::default() },
    )
    .map_err(|e| e.to_string())
}

fn edit_mask(target: &Value, size: [u32; 2]) -> Result<Option<Vec<u8>>, String> {
    let Some(data) = target.get("coverage").and_then(Value::as_str) else { return Ok(None) };
    let bytes = STANDARD.decode(data).map_err(|e| e.to_string())?;
    let image = decode_image(&bytes)?;
    if image.layout() != ChannelLayout::Gray || image.dimensions() != (size[0], size[1]) {
        return Err("Captured selection mask has invalid dimensions.".into());
    }
    let values = image.to_normalized();
    if values.iter().all(|v| *v >= 0.5) {
        return Ok(None);
    }
    let values: Vec<f32> = values.iter().map(|v| if *v >= 0.5 { 0.0 } else { 1.0 }).collect();
    let image = Image::from_normalized(size[0], size[1], ChannelLayout::Gray, SampleType::U8, &values).map_err(|e| e.to_string())?;
    photocraft_codecs::encode(&image, Format::Png, &EncodeOptions { embed_metadata: false, ..Default::default() }).map(Some).map_err(|e| e.to_string())
}

fn start(app: &mut PhotocraftApp, operation: &str) -> Result<Value, String> {
    if app.yorus_ai_runtime.busy() || app.codex_runtime.busy() {
        return Err("Wait for the current AI operation to finish, or stop it first.".into());
    }
    if !app.yorus_ai_runtime.pending.is_empty() {
        return Err("Place or discard the retained results first.".into());
    }
    if app.services.yorus_ai_generate.is_none() {
        return Err("YorusAi Direct is available in the native desktop build.".into());
    }
    normalize(&mut app.ui.yorus_ai);
    let s = &app.ui.yorus_ai;
    let model = match operation {
        "remix" => s.model.clone(),
        "upscale" => "crystal-upscaler".into(),
        "layers" => "seedream-5.5".into(),
        _ => return Err("Unknown YorusAi operation.".into()),
    };
    let route = match operation {
        "upscale" => "replicate",
        "layers" => "byteplus",
        _ => provider(&model, app.yorus_ai_runtime.keys),
    };
    if !has_key(app.yorus_ai_runtime.keys, route) {
        return Err(format!("Add your {} API key first.", provider_name(route)));
    }
    let prompt = if operation == "layers" { s.layer_prompt.clone() } else { s.prompt.clone() };
    if operation == "remix" && prompt.trim().is_empty() {
        return Err("Enter a prompt first.".into());
    }
    if prompt.len() > 64 << 10 || ((model == "seedream-5.5" || operation == "layers") && prompt.chars().count() > 4000) {
        return Err("This model accepts prompts up to 4,000 characters (64 KiB maximum).".into());
    }
    let (reference, mask, placement) = capture(app, operation)?;
    let input_size = if let Some(target) = &placement.target {
        let rect = target.get("rect").and_then(Value::as_array).ok_or("Capture returned no rectangle.")?;
        let value = |n| rect.get(n).and_then(Value::as_i64).ok_or("Invalid capture rectangle.");
        [
            u32::try_from(value(2)? - value(0)?).map_err(|_| "Invalid capture width.")?,
            u32::try_from(value(3)? - value(1)?).map_err(|_| "Invalid capture height.")?,
        ]
    } else if let Some(bytes) = &reference {
        let image = decode_image(bytes)?;
        [image.width(), image.height()]
    } else {
        placement.size
    };
    let s = &app.ui.yorus_ai;
    let cancel = Arc::new(AtomicBool::new(false));
    let request = DirectRequest {
        operation: operation.into(),
        model,
        prompt,
        ratio: s.ratio.clone(),
        resolution: if operation == "layers" { s.layer_resolution.clone() } else { s.resolution.clone() },
        quality: if s.model == "ideogram-4-5-precise-edit" { s.ideogram_quality.clone() } else { s.quality.clone() },
        scale_factor: s.scale_factor,
        reference,
        mask,
        references: if operation == "remix" { app.yorus_ai_runtime.references.iter().map(|r| r.bytes.clone()).collect() } else { Vec::new() },
        input_size,
        background: s.background.clone(),
        max_layers: s.layer_max,
        text_layers: s.layer_text,
        cancel: cancel.clone(),
    };
    let (tx, rx) = mpsc::channel();
    if let Some(generate) = &app.services.yorus_ai_generate {
        generate(request, tx);
    }
    app.yorus_ai_runtime.events = Some(rx);
    app.yorus_ai_runtime.cancel = Some(cancel);
    app.yorus_ai_runtime.placement = Some(placement);
    status(app, format!("Sending to {}…", provider_name(route)), false);
    Ok(json!({"started":true,"operation":operation}))
}

fn apply(app: &mut PhotocraftApp, image: &DirectImage) -> Result<Value, String> {
    let placement = app.yorus_ai_runtime.placement.as_ref().ok_or("No saved image destination.")?;
    let name: String = if image.name.trim().is_empty() { "YorusAi result".into() } else { image.name.chars().take(64).collect() };
    let data = STANDARD.encode(&image.bytes);
    let (command, params, document) = if let Some(target) = &placement.target {
        let fit = if app.ui.yorus_ai.grow_document { "grow" } else { "exact" };
        ("ai.apply", json!({"target":target,"data":data,"name":name,"fit":fit}), target.get("document").and_then(Value::as_u64))
    } else {
        ("ai.open", json!({"data":data,"name":name,"width":placement.size[0],"height":placement.size[1]}), None)
    };
    let previous = app.session.active_index();
    if let Some(document) = document {
        let index = app.session.documents().iter().position(|d| d.doc.id.0 == document).ok_or("The original canvas was closed; the results are retained.")?;
        if !app.session.set_active(index) {
            return Err("Could not activate the original canvas.".into());
        }
    }
    let result = app.session.execute(command, params).map_err(|e| e.to_string());
    if document.is_some()
        && let Some(index) = previous
    {
        app.session.set_active(index);
    }
    app.sync_views();
    if let Ok(value) = &result
        && let Some(placement) = app.yorus_ai_runtime.placement.as_mut()
        && let Some(target) = placement.target.as_mut()
    {
        // A grown document has a new size: the rest of this request uses the new target.
        if let Some(grown) = value.get("target") {
            *target = grown.clone();
        }
        if let Some(layer) = value.get("layers").and_then(Value::as_array).and_then(|layers| layers.last()).or_else(|| value.get("layer")) {
            target["anchorLayer"] = layer.clone();
        }
    }
    result
}

fn place_pending(app: &mut PhotocraftApp) {
    let mut count = 0;
    while let Some(image) = app.yorus_ai_runtime.pending.pop_front() {
        if let Err(error) = apply(app, &image) {
            app.yorus_ai_runtime.pending.push_front(image);
            status(app, format!("Results are ready. {error}"), true);
            return;
        }
        count += 1;
    }
    let operation = app.yorus_ai_runtime.placement.as_ref().map(|p| p.operation.as_str()).unwrap_or("remix");
    status(
        app,
        if operation == "layers" { format!("Added {count} positioned, separate layers.") } else { "Image ready in its saved destination.".into() },
        false,
    );
}

pub fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.yorus_ai_runtime.keys_loaded && app.yorus_ai_runtime.key_reply.is_none() {
        refresh_keys(app);
    }
    match app.yorus_ai_runtime.key_reply.as_ref().map(Receiver::try_recv) {
        Some(Ok(result)) => {
            app.yorus_ai_runtime.key_reply = None;
            app.yorus_ai_runtime.keys_loaded = true;
            match result {
                Ok(keys) => {
                    app.yorus_ai_runtime.keys = keys;
                    if app.ui.yorus_ai.status == "Saving provider key…" || app.ui.yorus_ai.status == "Removing provider key…" {
                        status(app, "Provider keys updated.", false);
                    }
                }
                Err(error) => status(app, error, true),
            }
        }
        Some(Err(TryRecvError::Disconnected)) => {
            app.yorus_ai_runtime.key_reply = None;
            app.yorus_ai_runtime.keys_loaded = true;
            status(app, "The provider key service closed its connection.", true);
        }
        _ => {}
    }
    loop {
        match app.yorus_ai_runtime.events.as_ref().map(Receiver::try_recv) {
            Some(Ok(DirectEvent::Status(message))) => status(app, message, false),
            Some(Ok(DirectEvent::Completed(mut images))) => {
                app.yorus_ai_runtime.events = None;
                if app.yorus_ai_runtime.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                    status(app, "Cancelled.", false);
                    break;
                }
                if images.is_empty() {
                    status(app, "The provider returned no images.", true);
                    break;
                }
                images.sort_by_key(|image| image.z_index);
                app.yorus_ai_runtime.pending.extend(images);
                place_pending(app);
                break;
            }
            Some(Ok(DirectEvent::Error(error))) => {
                app.yorus_ai_runtime.events = None;
                status(app, error, true);
                break;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                app.yorus_ai_runtime.events = None;
                status(app, "The provider connection closed before a result arrived.", true);
                break;
            }
            _ => break,
        }
    }
    if app.yorus_ai_runtime.busy() || app.yorus_ai_runtime.key_reply.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

fn refresh_keys(app: &mut PhotocraftApp) {
    if app.yorus_ai_runtime.key_reply.is_some() {
        return;
    }
    if let Some(load) = &app.services.yorus_ai_key_status {
        let (tx, rx) = mpsc::channel();
        load(tx);
        app.yorus_ai_runtime.key_reply = Some(rx);
    } else {
        app.yorus_ai_runtime.keys_loaded = true;
    }
}

fn save_key(app: &mut PhotocraftApp, remove: bool) -> Result<(), String> {
    if app.yorus_ai_runtime.key_reply.is_some() {
        return Err("Wait for the key operation to finish.".into());
    }
    let provider = app.yorus_ai_runtime.key_provider.clone();
    if !["replicate", "byteplus"].contains(&provider.as_str()) {
        return Err("Choose a provider first.".into());
    }
    let key = if remove { String::new() } else { std::mem::take(&mut app.yorus_ai_runtime.key_input).trim().to_owned() };
    if !remove && key.is_empty() {
        return Err("Paste an API key first.".into());
    }
    let save = app.services.yorus_ai_save_key.as_ref().ok_or("The native credential service is unavailable.")?;
    let (tx, rx) = mpsc::channel();
    save(provider, key, tx);
    app.yorus_ai_runtime.key_reply = Some(rx);
    status(app, if remove { "Removing provider key…" } else { "Saving provider key…" }, false);
    Ok(())
}

fn add_reference(app: &mut PhotocraftApp, name: String, bytes: Vec<u8>) -> Result<Value, String> {
    if app.yorus_ai_runtime.references.len() >= MAX_REFS {
        return Err("You can use up to 13 reference images.".into());
    }
    let total = app.yorus_ai_runtime.references.iter().map(|r| r.bytes.len()).sum::<usize>();
    if bytes.len() > MAX_REFERENCE_BYTES.saturating_sub(total) {
        return Err("Reference images together must fit within 64 MiB.".into());
    }
    let image = decode_image(&bytes)?;
    let size = [image.width(), image.height()];
    let name = name.rsplit(['/', '\\']).next().unwrap_or("Reference").chars().take(120).collect();
    app.yorus_ai_runtime.references.push(Reference { name, bytes, size, thumbnail: None });
    Ok(json!({"referenceCount":app.yorus_ai_runtime.references.len()}))
}

pub fn handle(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let canonical = id.replace("yorusAi", "yorusai");
    match canonical.as_str() {
        "window.yorusai" | "yorusai.open" => {
            app.ui.yorus_ai.open = true;
            crate::ai_sidebar::open(app, crate::ai_sidebar::Tab::YorusAi);
            Some(Ok(json!({"open":true})))
        }
        "yorusai.inspect" => Some(Ok(json!({"settings":app.ui.yorus_ai,"runtime":app.yorus_ai_runtime.inspect()}))),
        "yorusai.cancel" => {
            if let Some(cancel) = &app.yorus_ai_runtime.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            Some(Ok(json!({"stopping":app.yorus_ai_runtime.busy()})))
        }
        "yorusai.run" | "yorusai.remix" | "yorusai.upscale" | "yorusai.layers" => {
            let operation = if canonical == "yorusai.run" {
                match params.get("operation") {
                    Some(value) => match value.as_str().filter(|s| ["remix", "upscale", "layers"].contains(s)) {
                        Some(value) => value.to_owned(),
                        None => return Some(Err("operation must be remix, upscale or layers.".into())),
                    },
                    None => app.ui.yorus_ai.tab.clone(),
                }
            } else {
                canonical.strip_prefix("yorusai.").unwrap_or("remix").to_owned()
            };
            Some(configure(app, params, &operation).and_then(|()| start(app, &operation)))
        }
        _ => None,
    }
}

fn configure(app: &mut PhotocraftApp, params: &Value, operation: &str) -> Result<(), String> {
    if !params.is_object() {
        return Err("Operation parameters must be an object.".into());
    }
    let mut state = app.ui.yorus_ai.clone();
    if operation == "remix"
        && let Some(model) = params.get("model")
    {
        state.model = model.as_str().ok_or("model must be text.")?.into();
        if !MODELS.iter().any(|(model, _)| *model == state.model) {
            return Err("Choose a supported YorusAi model.".into());
        }
        normalize(&mut state);
    }
    let fields = if operation == "remix" {
        vec![
            ("prompt", &mut state.prompt),
            ("ratio", &mut state.ratio),
            ("resolution", &mut state.resolution),
            ("quality", &mut state.quality),
            ("background", &mut state.background),
        ]
    } else if operation == "layers" {
        vec![("prompt", &mut state.layer_prompt), ("resolution", &mut state.layer_resolution)]
    } else {
        Vec::new()
    };
    for (key, output) in fields {
        if let Some(value) = params.get(key) {
            *output = value.as_str().filter(|s| s.len() <= 64 << 10).ok_or_else(|| format!("{key} must be bounded text."))?.into();
        }
    }
    for (key, output) in [("layerPrompt", &mut state.layer_prompt), ("layerResolution", &mut state.layer_resolution)] {
        if let Some(value) = params.get(key) {
            *output = value.as_str().filter(|s| s.len() <= 64 << 10).ok_or_else(|| format!("{key} must be bounded text."))?.into();
        }
    }
    if operation == "layers" {
        if let Some(value) = params.get("maxLayers") {
            state.layer_max = value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (2..=MAX_LAYER_IMAGES).contains(n))
                .ok_or("maxLayers must be a whole number from 2 to 100.")?;
        }
        if let Some(value) = params.get("textLayers") {
            state.layer_text = value.as_bool().ok_or("textLayers must be true or false.")?;
        }
    }
    if let Some(value) = params.get("mode") {
        let new_canvas = match value.as_str() {
            Some("selection") => false,
            Some("canvas") => true,
            _ => return Err("mode must be selection or canvas.".into()),
        };
        if operation == "remix" {
            state.new_canvas = new_canvas;
        } else if operation == "upscale" {
            state.upscale_new_canvas = new_canvas;
        } else if new_canvas {
            return Err("Layer Separation uses the captured area of the original canvas.".into());
        }
    }
    for (key, output) in [("width", &mut state.width), ("height", &mut state.height), ("scaleFactor", &mut state.scale_factor)] {
        if let Some(value) = params.get(key) {
            *output = value.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| format!("{key} must be a positive integer."))?;
        }
    }
    if !MODELS.iter().any(|(model, _)| *model == state.model) {
        return Err("Choose a supported YorusAi model.".into());
    }
    if operation == "remix" && !ratios(&state.model).contains(&state.ratio.as_str()) {
        return Err("This model does not support the requested ratio.".into());
    }
    if operation == "remix" && !resolutions(&state.model).contains(&state.resolution.as_str()) {
        return Err("This model does not support the requested resolution.".into());
    }
    if !(2..=8).contains(&state.scale_factor) {
        return Err("Upscale factor must be between 2 and 8.".into());
    }
    let allowed_quality: &[&str] = if state.model == "ideogram-4-5-precise-edit" { &["very_low", "low", "medium", "high"] } else { &["low", "medium", "high"] };
    if operation == "remix" && !allowed_quality.contains(&state.quality.as_str()) {
        return Err("This model does not support the requested quality.".into());
    }
    if operation == "remix" && state.new_canvas && state.model == "ideogram-4-5-precise-edit" {
        return Err("Ideogram Precise Edit requires a selected area.".into());
    }
    if state.model == "ideogram-4-5-precise-edit"
        && let Some(quality) = params.get("quality").and_then(Value::as_str)
    {
        state.ideogram_quality = quality.into();
    }
    if operation == "layers" && !["1K", "1.5K", "2K"].contains(&state.layer_resolution.as_str()) {
        return Err("Layer resolution must be 1K, 1.5K or 2K.".into());
    }
    if operation == "remix" && !BACKGROUNDS.contains(&state.background.as_str()) {
        return Err("background must be opaque, transparent or auto.".into());
    }
    state.open = true;
    state.tab = operation.into();
    app.ui.yorus_ai = state;
    Ok(())
}

fn string_options(ui: &mut egui::Ui, id: &str, label: &str, current: &mut String, options: &[&str]) -> bool {
    ui.label(label);
    let labels: Vec<String> = options
        .iter()
        .map(|v| match *v {
            "auto" => "Auto".into(),
            "opaque" => "Opaque".into(),
            "transparent" => "Transparent (PNG with alpha)".into(),
            "very_low" => "Very low".into(),
            "low" => "Low".into(),
            "medium" => "Medium".into(),
            "high" => "High".into(),
            "768sq" => "768 px".into(),
            "1k" => "1K".into(),
            "1.5k" => "1.5K".into(),
            "2k" => "2K".into(),
            "4k" => "4K".into(),
            other => other.into(),
        })
        .collect();
    let choices: Vec<(String, &str)> = options.iter().zip(labels.iter()).map(|(value, label)| ((*value).into(), label.as_str())).collect();
    let width = ui.available_width();
    crate::widgets::dropdown(ui, id, label, current, &choices, width)
}

fn preset_size(state: &State) -> [u32; 2] {
    // Auto keeps the canvas size typed in; a ratio sets it from the resolution's long edge.
    if state.ratio == "auto" {
        return [state.width, state.height];
    }
    let edge = match state.resolution.to_lowercase().as_str() {
        "768sq" => 768.0,
        "1.5k" => 1536.0,
        "2k" => 2048.0,
        "4k" => 4096.0,
        _ => 1024.0,
    };
    let ratio = state
        .ratio
        .split_once(':')
        .and_then(|(w, h)| Some(w.parse::<f64>().ok()? / h.parse::<f64>().ok()?))
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(1.0);
    if ratio >= 1.0 { [edge as u32, (edge / ratio).round().max(1.0) as u32] } else { [(edge * ratio).round().max(1.0) as u32, edge as u32] }
}

fn make_prompt(builder: &PromptBuilder) -> String {
    let preservation = "Preserve the exact composition, camera, geometry, proportions, openings, and existing objects. Do not add, remove, or reposition design elements; transform their representation only.";
    if builder.style_reference {
        format!(
            "IMAGE A is a {} of a {}. IMAGE B is the first additional reference. Use IMAGE B only for visualization style, lighting, palette, rendering technique, and mood. Do not copy its design, forms, or materials.\n\n{}\n\nARCHITECTURE STYLE: {}\nMATERIALS: {}",
            builder.input, builder.project, preservation, builder.style, builder.materials
        )
    } else {
        format!(
            "Transform the provided {} into a {} of a {}.\n\n{}\n\nLIGHTING: {}\nARCHITECTURE STYLE: {}\nMATERIALS: {}\nATMOSPHERE: {}",
            builder.input, builder.output, builder.project, preservation, builder.lighting, builder.style, builder.materials, builder.atmosphere
        )
    }
}

fn builder_ui(ui: &mut egui::Ui, state: &mut State, references: usize) {
    ui.collapsing("Prompt builder", |ui| {
        let builder = &mut state.builder;
        for (label, value) in [
            ("Input image", &mut builder.input),
            ("Project", &mut builder.project),
            ("Turn into", &mut builder.output),
            ("Lighting", &mut builder.lighting),
            ("Architecture style", &mut builder.style),
            ("Materials", &mut builder.materials),
            ("Atmosphere", &mut builder.atmosphere),
        ] {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY).char_limit(500));
        }
        crate::widgets::checkbox(ui, &mut builder.style_reference, "Use first reference for visual style");
        if builder.style_reference && references == 0 {
            ui.small("Add a reference image to use style transfer.");
        }
        if ui.add_enabled(!builder.style_reference || references > 0, egui::Button::new("Use this prompt")).clicked() {
            state.prompt = make_prompt(builder);
        }
    });
}

fn reference_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::ai_sidebar::palette(ui);
    ui.horizontal(|ui| {
        ui.label("References");
        ui.label(egui::RichText::new(format!("{}/13", app.yorus_ai_runtime.references.len())).color(t.text_faint));
    });
    let mut remove = None;
    for (index, reference) in app.yorus_ai_runtime.references.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                if reference.thumbnail.is_none()
                    && let Ok(image) = decode_image(&reference.bytes)
                {
                    let pixels = image.to_rgba8();
                    let width = image.width();
                    let height = image.height();
                    let scale = 48.0 / f64::from(width.max(height).max(1));
                    let tw = (f64::from(width) * scale).round().max(1.0) as usize;
                    let th = (f64::from(height) * scale).round().max(1.0) as usize;
                    let mut preview = Vec::with_capacity(tw.saturating_mul(th).saturating_mul(4));
                    for y in 0..th {
                        for x in 0..tw {
                            let sy = ((y as f64 / th as f64) * f64::from(height)).floor() as usize;
                            let sx = ((x as f64 / tw as f64) * f64::from(width)).floor() as usize;
                            let at = sy.saturating_mul(width as usize).saturating_add(sx).saturating_mul(4);
                            preview.extend_from_slice(pixels.get(at..at.saturating_add(4)).unwrap_or(&[0, 0, 0, 0]));
                        }
                    }
                    reference.thumbnail = Some(ui.ctx().load_texture(
                        format!("yorus-reference-{index}"),
                        egui::ColorImage::from_rgba_unmultiplied([tw, th], &preview),
                        egui::TextureOptions::LINEAR,
                    ));
                }
                if let Some(texture) = &reference.thumbnail {
                    ui.image((texture.id(), texture.size_vec2()));
                }
                ui.vertical(|ui| {
                    let width = (ui.available_width() - 34.0).max(40.0);
                    crate::widgets::elided_label(ui, &reference.name, width);
                    ui.small(format!("{} × {}", reference.size[0], reference.size[1]));
                });
                if ui.small_button("×").on_hover_text("Remove reference").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        app.yorus_ai_runtime.references.remove(index);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(app.yorus_ai_runtime.references.len() < MAX_REFS && !app.file_dialog_open(), egui::Button::new("Add image…")).clicked()
            && let Err(error) = app.pick_file_bytes_filtered(&["png", "jpg", "jpeg", "webp"], add_reference)
        {
            status(app, error, true);
        }
        let can_capture = app.session.active().is_some() && app.yorus_ai_runtime.references.len() < MAX_REFS;
        if ui.add_enabled(can_capture, egui::Button::new("From canvas")).clicked() {
            let result = app.run("ai.capture", json!({})).and_then(|value| {
                let data = value.get("reference").and_then(Value::as_str).ok_or("Capture returned no reference.")?;
                let bytes = STANDARD.decode(data).map_err(|e| e.to_string())?;
                add_reference(app, "Canvas reference".into(), bytes)
            });
            if let Err(error) = result {
                status(app, error, true);
            }
        }
    });
}

fn key_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::ai_sidebar::palette(ui);
    ui.label(egui::RichText::new("API keys").font(crate::theme::semibold(16.0)));
    ui.label("Use your own provider accounts. Keys are kept by the native credential service.");
    let current = app.yorus_ai_runtime.key_provider.clone();
    if current.is_empty() {
        app.yorus_ai_runtime.key_provider = "replicate".into();
    }
    if !["replicate", "byteplus"].contains(&app.yorus_ai_runtime.key_provider.as_str()) {
        app.yorus_ai_runtime.key_provider = "replicate".into();
    }
    let options = [("replicate".into(), "Replicate"), ("byteplus".into(), "BytePlus ModelArk")];
    let width = ui.available_width();
    crate::widgets::dropdown(ui, "yorus-key-provider", "Provider", &mut app.yorus_ai_runtime.key_provider, &options, width);
    if current != app.yorus_ai_runtime.key_provider {
        app.yorus_ai_runtime.key_input.clear();
    }
    let provider = &app.yorus_ai_runtime.key_provider;
    ui.colored_label(t.text_dim, if has_key(app.yorus_ai_runtime.keys, provider) { "Key saved" } else { "No key saved" });
    ui.small(match provider.as_str() {
        "byteplus" => "Seedream 5.5 and Layer Separation. Activate Seedream 5.5 in the ModelArk console first.",
        _ => "Nano Banana, Ideogram, FLUX, Seedream 5.0 Pro and Crystal Upscale",
    });
    ui.add(
        egui::TextEdit::singleline(&mut app.yorus_ai_runtime.key_input).password(true).hint_text("Paste API key").char_limit(4096).desired_width(f32::INFINITY),
    );
    let working = app.yorus_ai_runtime.key_reply.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!working, egui::Button::new("Save key")).clicked()
            && let Err(error) = save_key(app, false)
        {
            status(app, error, true);
        }
        if ui.add_enabled(!working, egui::Button::new("Remove")).clicked()
            && let Err(error) = save_key(app, true)
        {
            status(app, error, true);
        }
        if ui.add_enabled(!working, egui::Button::new("Refresh")).clicked() {
            refresh_keys(app);
        }
        if working {
            ui.spinner();
        }
    });
    if ui.button("Back to tools").clicked() {
        app.yorus_ai_runtime.show_keys = false;
        app.yorus_ai_runtime.key_input.clear();
    }
}

/// Draw inside the shared native AI sidebar. No separate window or web content is created.
pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    app.ui.yorus_ai.open = true;
    let t = crate::ai_sidebar::palette(ui);
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("YorusAi").font(crate::theme::semibold(20.0)).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("API keys").clicked() {
                app.yorus_ai_runtime.show_keys = !app.yorus_ai_runtime.show_keys;
                refresh_keys(app);
            }
        });
    });
    ui.label(egui::RichText::new("Direct image tools").color(t.text_dim));
    if !app.ui.yorus_ai.status.is_empty() {
        ui.label(egui::RichText::new(&app.ui.yorus_ai.status).color(if app.ui.yorus_ai.error { t.danger } else { t.text_dim }));
    }
    if app.yorus_ai_runtime.busy() {
        ui.horizontal(|ui| {
            ui.spinner();
            if ui.button("Stop").clicked() {
                if let Some(cancel) = &app.yorus_ai_runtime.cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                status(app, "Stopping provider request…", false);
            }
        });
    }
    if !app.yorus_ai_runtime.pending.is_empty() {
        ui.label(format!("{} retained result(s)", app.yorus_ai_runtime.pending.len()));
        ui.horizontal_wrapped(|ui| {
            if ui.button("Retry placement").clicked() {
                place_pending(app);
            }
            if ui.button("Open in new canvases").clicked() {
                if let Some(placement) = &mut app.yorus_ai_runtime.placement {
                    placement.target = None;
                }
                place_pending(app);
            }
            if ui.button("Discard").clicked() {
                app.yorus_ai_runtime.pending.clear();
            }
        });
    }
    egui::ScrollArea::vertical().id_salt("yorus-ai-body").auto_shrink([false, false]).show(ui, |ui| {
        if app.yorus_ai_runtime.show_keys {
            key_ui(app, ui);
            return;
        }
        let busy = app.yorus_ai_runtime.busy() || app.codex_runtime.busy();
        let mut state = app.ui.yorus_ai.clone();
        normalize(&mut state);
        ui.horizontal_wrapped(|ui| {
            for (id, label) in [("remix", "Remix"), ("upscale", "Upscale"), ("layers", "Layers")] {
                if ui
                    .add(egui::Button::new(label).fill(if state.tab == id { t.hover } else { t.canvas }).stroke(egui::Stroke::NONE).corner_radius(t.radius_lg))
                    .clicked()
                {
                    state.tab = id.into();
                }
            }
        });
        crate::widgets::hairline(ui);
        let operation = state.tab.clone();
        let mut run = false;
        ui.add_enabled_ui(!busy, |ui| match operation.as_str() {
            "upscale" => {
                ui.label(egui::RichText::new("Crystal Upscale").font(crate::theme::semibold(16.0)));
                ui.label("Restore detail in the selected area.");
                let choices = (2..=8).map(|factor| (factor, format!("{factor}×"))).collect::<Vec<_>>();
                let options = choices.iter().map(|(n, label)| (*n, label.as_str())).collect::<Vec<_>>();
                ui.label("Scale factor");
                let width = ui.available_width();
                crate::widgets::dropdown(ui, "yorus-upscale-factor", "Scale factor", &mut state.scale_factor, &options, width);
                crate::widgets::checkbox(ui, &mut state.upscale_new_canvas, "Open at larger size in a new canvas");
                if !state.upscale_new_canvas {
                    crate::widgets::checkbox(ui, &mut state.grow_document, "Grow canvas to fit")
                        .on_hover_text("Keep every upscaled pixel: the whole canvas is enlarged and the layer keeps its place and size.");
                    ui.small(if state.grow_document {
                        "The canvas is enlarged as a whole; the layer keeps its place and size."
                    } else {
                        "The new layer fills the original selected rectangle."
                    });
                }
            }
            "layers" => {
                ui.label(egui::RichText::new("Layer Separation").font(crate::theme::semibold(16.0)));
                ui.label("Separate subjects, text, background and decorations into positioned layers.");
                ui.small("Uses the selected area when present, otherwise the visible canvas.");
                ui.label("Instructions · optional");
                ui.add(
                    egui::TextEdit::multiline(&mut state.layer_prompt)
                        .hint_text("Separate all main subjects and background…")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .char_limit(4000),
                );
                string_options(ui, "yorus-layers-size", "Output resolution", &mut state.layer_resolution, &["1K", "1.5K", "2K"]);
                ui.label("Most layers");
                ui.add(egui::Slider::new(&mut state.layer_max, 2..=MAX_LAYER_IMAGES).text("images"));
                ui.small("The base counts as one; each returned layer is billed.");
                crate::widgets::checkbox(ui, &mut state.layer_text, "Name text layers by their text")
                    .on_hover_text("Seedream reads the text in each text layer and names the layer after it.");
            }
            _ => {
                ui.label("Model");
                let labels: Vec<(String, String)> = MODELS
                    .iter()
                    .map(|(id, label)| {
                        let route = provider(id, app.yorus_ai_runtime.keys);
                        let available = has_key(app.yorus_ai_runtime.keys, route);
                        ((*id).into(), if available { (*label).into() } else { format!("{label} · add key") })
                    })
                    .collect();
                let options: Vec<(String, &str)> = labels.iter().map(|(id, label)| (id.clone(), label.as_str())).collect();
                let width = ui.available_width();
                let changed_model = crate::widgets::dropdown(ui, "yorus-model", "Model", &mut state.model, &options, width);
                normalize(&mut state);
                let previous_destination = state.new_canvas;
                ui.add_enabled_ui(state.model != "ideogram-4-5-precise-edit", |ui| {
                    crate::widgets::checkbox(ui, &mut state.new_canvas, "Generate into a new canvas");
                });
                if state.new_canvas {
                    ui.small("Uses your prompt and additional references.");
                } else {
                    crate::widgets::checkbox(ui, &mut state.grow_document, "Grow canvas to fit")
                        .on_hover_text("When the result has more pixels than the selection, enlarge the whole canvas instead of shrinking the result. The layer keeps its place and size.");
                }
                if state.model == "ideogram-4-5-precise-edit" {
                    ui.small("Edits the selection's exact shape and keeps its original framing.");
                    string_options(ui, "yorus-ideogram-quality", "Quality", &mut state.ideogram_quality, &["very_low", "low", "medium", "high"]);
                } else {
                    let changed_resolution = string_options(ui, "yorus-resolution", "Resolution", &mut state.resolution, resolutions(&state.model));
                    let changed_ratio = string_options(ui, "yorus-ratio", "Aspect ratio", &mut state.ratio, &ratios(&state.model));
                    if state.model == "seedream-5.5" {
                        string_options(ui, "yorus-background", "Background", &mut state.background, BACKGROUNDS);
                        if state.background == "transparent" {
                            ui.small("Returns a PNG with alpha: sprites, icons and effect textures.");
                        }
                        if state.ratio == "auto" {
                            ui.small("Auto follows the selection or canvas shape.");
                        }
                        if provider(&state.model, app.yorus_ai_runtime.keys) == "replicate" {
                            ui.small("Seedream 5.5 needs a BytePlus key; with Replicate this runs Seedream 5.0 Pro.");
                        }
                    }
                    if state.new_canvas {
                        if changed_model || changed_ratio || changed_resolution || state.new_canvas != previous_destination {
                            [state.width, state.height] = preset_size(&state);
                        }
                        ui.label("Canvas size");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut state.width).range(1..=16_384).suffix(" px"));
                            ui.label("×");
                            ui.add(egui::DragValue::new(&mut state.height).range(1..=16_384).suffix(" px"));
                        });
                    }
                }
                ui.label("Prompt");
                ui.add(
                    egui::TextEdit::multiline(&mut state.prompt)
                        .hint_text("Describe the image or edit…")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .char_limit(if state.model == "seedream-5.5" { 4000 } else { 16000 }),
                );
                builder_ui(ui, &mut state, app.yorus_ai_runtime.references.len());
            }
        });
        app.ui.yorus_ai = state.clone();
        if operation == "remix" {
            reference_ui(app, ui);
            if state.model == "ideogram-4-5-precise-edit" {
                ui.small("Ideogram uses up to 3 references with a selection mask, or 4 without one.");
            } else if matches!(state.model.as_str(), "flux-3" | "seedream-5.5")
                && app.yorus_ai_runtime.references.len() > if state.new_canvas { 10 } else { 9 }
            {
                ui.small("This model uses the first 10 input images, including the selected area.");
            }
        }
        if !(operation == "remix" && state.new_canvas) {
            if let Some(document) = app.session.active() {
                let area = document.doc.selection.as_ref().map(|s| s.content_bounds().intersect(&document.doc.bounds()));
                if let Some(area) = area.filter(|r| !r.is_empty()) {
                    ui.small(format!("Selection: {} × {} px at ({}, {})", area.width(), area.height(), area.x0, area.y0));
                    if operation == "upscale" && state.upscale_new_canvas {
                        ui.small(format!(
                            "New canvas: {} × {} px",
                            u64::from(area.width()) * u64::from(state.scale_factor),
                            u64::from(area.height()) * u64::from(state.scale_factor)
                        ));
                    }
                } else if operation == "layers" {
                    ui.small(format!("Canvas: {} × {} px", document.doc.size.width, document.doc.size.height));
                } else {
                    ui.label(egui::RichText::new("Select an area on the canvas first.").color(t.text_dim));
                }
            } else {
                ui.label("Open an image first.");
            }
        }
        let route = match operation.as_str() {
            "upscale" => "replicate",
            "layers" => "byteplus",
            _ => provider(&state.model, app.yorus_ai_runtime.keys),
        };
        ui.small(format!("Billed to your {} API account.", provider_name(route)));
        if operation == "remix" && state.model == "nano-banana-2.1" {
            ui.small("Automatically retries once with Nano Banana 2 if the primary model fails.");
        }
        let enabled = !busy
            && app.yorus_ai_runtime.pending.is_empty()
            && app.services.yorus_ai_generate.is_some()
            && has_key(app.yorus_ai_runtime.keys, route)
            && (operation != "remix" || !state.prompt.trim().is_empty());
        let label = match operation.as_str() {
            "upscale" => "Upscale selection",
            "layers" => "Separate layers",
            _ => "Generate remix",
        };
        ui.add_enabled_ui(enabled, |ui| {
            let width = ui.available_width();
            run = ui.add_sized([width, 36.0], egui::Button::new(egui::RichText::new(label).color(t.canvas)).fill(t.text).corner_radius(t.radius_lg)).clicked();
        });
        if !has_key(app.yorus_ai_runtime.keys, route) && ui.button(format!("Add {} key", provider_name(route))).clicked() {
            app.yorus_ai_runtime.show_keys = true;
            app.yorus_ai_runtime.key_provider = route.into();
        }
        if run && let Err(error) = start(app, &operation) {
            status(app, error, true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_geom::Rect;
    use photocraft_raster::Surface;

    fn png(color: [u8; 4]) -> Vec<u8> {
        let image = Image::from_raw(3, 2, ChannelLayout::Rgba, SampleType::U8, color.repeat(6)).unwrap();
        photocraft_codecs::encode(&image, Format::Png, &Default::default()).unwrap()
    }

    fn result(name: &str, z_index: i32) -> DirectImage {
        DirectImage { bytes: png([20, 180, 90, 255]), name: name.into(), z_index, bounds: None }
    }

    fn app(generate: impl Fn(DirectRequest, Sender<DirectEvent>) + 'static) -> PhotocraftApp {
        let mut app =
            PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services { yorus_ai_generate: Some(Box::new(generate)), ..Default::default() });
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.run("select.rect", json!({"x":5,"y":7,"width":12,"height":10})).unwrap();
        app.yorus_ai_runtime.keys = DirectKeys { replicate: true, byteplus: true };
        app.yorus_ai_runtime.keys_loaded = true;
        app.ui.yorus_ai.prompt = "Restyle the selected image".into();
        app
    }

    fn ready_app() -> PhotocraftApp {
        app(|_, sender| sender.send(DirectEvent::Completed(vec![result("YorusAi result", 0)])).unwrap())
    }

    #[test]
    fn selected_result_keeps_original_geometry_and_source_pixels_in_one_undo() {
        let mut app = app(|request, sender| {
            assert_eq!(request.operation, "remix");
            assert_eq!(request.input_size, [12, 10]);
            let reference = decode_image(request.reference.as_ref().unwrap()).unwrap();
            assert_eq!(reference.dimensions(), (12, 10));
            sender.send(DirectEvent::Completed(vec![result("Remixed", 0)])).unwrap();
        });
        let original = app.session.active().unwrap().doc.layers[0].clone();
        start(&mut app, "remix").unwrap();
        app.run("select.rect", json!({"x":30,"y":20,"width":2,"height":3})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.yorus_ai.error, "{}", app.ui.yorus_ai.status);
        let state = app.session.active().unwrap();
        assert_eq!(state.doc.layers.len(), 2);
        assert_eq!(state.doc.layers[0], original);
        assert_eq!(state.doc.layers[1].surface().unwrap().content_bounds(), Rect::new(5, 7, 17, 17));
        assert_eq!(state.doc.selection.as_ref().unwrap().content_bounds(), Rect::new(30, 20, 32, 23));
        assert_eq!(state.history.past_len(), history + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }

    #[test]
    fn direct_reference_echo_keeps_odd_offset_pixels_and_original_irregular_coverage() {
        let mut app = app(|request, sender| {
            assert_eq!(request.input_size, [17, 11]);
            sender
                .send(DirectEvent::Completed(vec![DirectImage {
                    bytes: request.reference.unwrap(),
                    name: "Exact reference echo".into(),
                    z_index: 0,
                    bounds: None,
                }]))
                .unwrap();
        });
        let area = Rect::new(9, 13, 26, 24);
        app.session
            .edit("Pattern and fractional selection", |doc, _| {
                let mut surface = Surface::new(doc.pixel_format());
                let mut pixels = Vec::new();
                for y in 0..30 {
                    for x in 0..40 {
                        pixels.extend([x as f32 / 39.0, y as f32 / 29.0, (x + y) as f32 / 68.0, 1.0]);
                    }
                }
                surface.try_write_region(Rect::new(0, 0, 40, 30), &pixels).unwrap();
                doc.layers[0].content = photocraft_doc::LayerContent::Raster(surface);
                let mut selection =
                    Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::U16, false));
                selection.fill_rect(area, &[1.0]);
                selection.fill_rect(Rect::new(14, 16, 16, 18), &[0.0]);
                selection.fill_rect(Rect::new(9, 13, 10, 24), &[0.25]);
                doc.selection = Some(selection);
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.layers[0].clone();
        let coverage = app.session.active().unwrap().doc.selection.clone().unwrap();
        start(&mut app, "remix").unwrap();
        app.run("select.rect", json!({"x":30,"y":1,"width":3,"height":3})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.yorus_ai.error, "{}", app.ui.yorus_ai.status);
        let state = app.session.active().unwrap();
        let placed = state.doc.layers[1].surface().unwrap();
        assert_eq!(placed.content_bounds(), area);
        assert_eq!(state.doc.layers[0], source);
        assert_eq!(state.doc.selection.as_ref().unwrap().content_bounds(), Rect::new(30, 1, 33, 4));
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let pixel = placed.rgba(x, y);
                let expected = source.surface().unwrap().rgba(x, y);
                let alpha = coverage.rgba(x, y)[0];
                assert!((pixel[3] - alpha).abs() < 0.005, "original selection alpha at ({x},{y})");
                if alpha > 0.0 {
                    for channel in 0..3 {
                        assert!((pixel[channel] - expected[channel]).abs() < 0.005, "reference pixel shifted at ({x},{y}), channel {channel}");
                    }
                }
            }
        }
        assert_eq!(state.history.past_len(), history + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }

    #[test]
    fn direct_png_jpeg_webp_results_fill_saved_selection_or_explicit_canvas_without_offsets() {
        for (width, height, format) in [(3, 7, Format::Png), (53, 29, Format::Jpeg), (17, 11, Format::WebP)] {
            let mut pixels = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    pixels.extend(match (x < width / 2, y < height / 2) {
                        (true, true) => [255, 0, 0],
                        (false, true) => [0, 255, 0],
                        (true, false) => [0, 0, 255],
                        (false, false) => [255, 255, 0],
                    });
                }
            }
            let image = Image::from_u8(width, height, ChannelLayout::Rgb, pixels).unwrap();
            let bytes = photocraft_codecs::encode(&image, format, &Default::default()).unwrap();
            for (new_canvas, grow) in [(false, false), (false, true), (true, false)] {
                let bytes = bytes.clone();
                let mut app = app(move |_, sender| {
                    sender
                        .send(DirectEvent::Completed(vec![DirectImage { bytes: bytes.clone(), name: "Aspect check".into(), z_index: 0, bounds: None }]))
                        .unwrap();
                });
                app.ui.yorus_ai.new_canvas = new_canvas;
                app.ui.yorus_ai.grow_document = grow;
                app.ui.yorus_ai.width = 31;
                app.ui.yorus_ai.height = 19;
                let source = app.session.active().unwrap().doc.clone();
                start(&mut app, "remix").unwrap();
                poll(&mut app, &egui::Context::default());
                assert!(!app.ui.yorus_ai.error, "{format:?}: {}", app.ui.yorus_ai.status);
                let document = &app.session.active().unwrap().doc;
                // A result with more pixels than the 12 x 10 selection grows the whole 40 x 30
                // canvas by one factor; the selection keeps its place relative to the canvas.
                let k = f64::from(document.size.width) / 40.0;
                let grows = grow && (width > 12 || height > 10);
                assert_eq!(k > 1.0, grows, "{format:?}, grow={grow}");
                let scaled = |v: i32| (f64::from(v) * k).round() as i32;
                let area = if new_canvas { Rect::new(0, 0, 31, 19) } else { Rect::new(scaled(5), scaled(7), scaled(17), scaled(17)) };
                let surface = document.layers.last().unwrap().surface().unwrap();
                let bounds = surface.content_bounds();
                for (got, want) in [(bounds.x0, area.x0), (bounds.y0, area.y0), (bounds.x1, area.x1), (bounds.y1, area.y1)] {
                    assert!((got - want).abs() <= 1, "{format:?}, canvas={new_canvas}, grow={grow}: {bounds:?} vs {area:?}");
                }
                if grows {
                    assert!(bounds.width() >= width && bounds.height() >= height, "every generated pixel is kept");
                }
                let area = bounds;
                for (x, y, color) in [
                    (area.x0, area.y0, [1.0, 0.0, 0.0]),
                    (area.x1 - 1, area.y0, [0.0, 1.0, 0.0]),
                    (area.x0, area.y1 - 1, [0.0, 0.0, 1.0]),
                    (area.x1 - 1, area.y1 - 1, [1.0, 1.0, 0.0]),
                ] {
                    let pixel = surface.rgba(x, y);
                    assert!(pixel[3] > 0.99);
                    for channel in 0..3 {
                        assert!((pixel[channel] - color[channel]).abs() < 0.15, "{format:?} image corner shifted at ({x},{y})");
                    }
                }
                if new_canvas {
                    assert_eq!(app.session.documents().len(), 2);
                    assert_eq!(app.session.documents()[0].doc, source);
                    assert_eq!((document.size.width, document.size.height), (31, 19));
                } else if !grows {
                    assert_eq!(document.layers[0], source.layers[0]);
                } else {
                    assert_eq!(document.layers.len(), source.layers.len() + 1, "the base layer is resampled, not replaced");
                }
            }
        }
    }

    #[test]
    fn tab_switch_places_into_source_and_restores_current_tab() {
        let mut app = ready_app();
        start(&mut app, "remix").unwrap();
        app.run("file.new", json!({"width":20,"height":20})).unwrap();
        let visible = app.session.active().unwrap().doc.id;
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.active().unwrap().doc.id, visible);
        assert_eq!(app.session.documents()[0].doc.layers.len(), 2);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }

    #[test]
    fn fresh_canvas_uses_only_explicit_references_and_exact_dimensions() {
        let mut app = app(|request, sender| {
            assert!(request.reference.is_none());
            assert_eq!(request.references.len(), 1);
            assert_eq!(request.input_size, [128, 96]);
            sender.send(DirectEvent::Completed(vec![result("Fresh image", 0)])).unwrap();
        });
        add_reference(&mut app, "style.png".into(), png([100, 50, 80, 255])).unwrap();
        app.ui.yorus_ai.new_canvas = true;
        app.ui.yorus_ai.width = 128;
        app.ui.yorus_ai.height = 96;
        let original = app.session.active().unwrap().doc.clone();
        start(&mut app, "remix").unwrap();
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.documents().len(), 2);
        assert_eq!(app.session.documents()[0].doc, original);
        let document = &app.session.active().unwrap().doc;
        assert_eq!((document.size.width, document.size.height), (128, 96));
    }

    #[test]
    fn crystal_can_open_exact_upscaled_canvas_with_selection_reference() {
        let mut app = app(|request, sender| {
            assert_eq!(request.operation, "upscale");
            assert_eq!(request.model, "crystal-upscaler");
            assert_eq!(request.scale_factor, 3);
            assert_eq!(request.input_size, [12, 10]);
            assert!(request.reference.is_some());
            sender.send(DirectEvent::Completed(vec![result("Upscaled", 0)])).unwrap();
        });
        app.ui.yorus_ai.upscale_new_canvas = true;
        app.ui.yorus_ai.scale_factor = 3;
        start(&mut app, "upscale").unwrap();
        poll(&mut app, &egui::Context::default());
        let document = &app.session.active().unwrap().doc;
        assert_eq!((document.size.width, document.size.height), (36, 30));
    }

    #[test]
    fn decomposition_respects_selection_mask_and_ascending_layer_order() {
        let mut app = app(|request, sender| {
            assert_eq!(request.operation, "layers");
            assert_eq!(request.input_size, [12, 10]);
            assert!(request.mask.is_none(), "only Ideogram uses the provider edit mask");
            sender.send(DirectEvent::Completed(vec![result("Foreground", 8), result("Base", 0), result("Middle", 4)])).unwrap();
        });
        app.session
            .edit("Nonrectangular selection", |doc, _| {
                let mut mask = Surface::new(photocraft_color::PixelFormat::GRAY8);
                mask.fill_rect(Rect::new(5, 7, 17, 17), &[1.0]);
                mask.fill_rect(Rect::new(8, 9, 10, 11), &[0.0]);
                doc.selection = Some(mask);
                Ok(())
            })
            .unwrap();
        start(&mut app, "layers").unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.yorus_ai.error, "{}", app.ui.yorus_ai.status);
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["Background", "Base", "Middle", "Foreground"]);
        for layer in doc.layers.iter().skip(1) {
            let surface = layer.surface().unwrap();
            assert_eq!(surface.content_bounds(), Rect::new(5, 7, 17, 17));
            assert!(surface.rgba(9, 10)[3] < 0.001);
            assert!(surface.rgba(6, 8)[3] > 0.99);
            assert!(surface.rgba(18, 18)[3] < 0.001);
        }
    }

    #[test]
    fn decomposition_without_selection_captures_visible_canvas() {
        let mut app = app(|request, sender| {
            assert_eq!(request.input_size, [40, 30]);
            sender.send(DirectEvent::Completed(vec![result("Separated base", 0)])).unwrap();
        });
        app.run("select.deselect", json!({})).unwrap();
        start(&mut app, "layers").unwrap();
        poll(&mut app, &egui::Context::default());
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers[1].surface().unwrap().content_bounds(), Rect::new(0, 0, 40, 30));
    }

    #[test]
    fn ideogram_receives_black_edit_white_preserve_mask() {
        let mut app = app(|request, sender| {
            assert_eq!(request.quality, "very_low");
            let image = decode_image(request.mask.as_ref().unwrap()).unwrap();
            assert_eq!(image.dimensions(), (12, 10));
            assert!(image.get(0, 0, 0) < 0.001);
            assert!(image.get(4, 3, 0) > 0.999);
            sender.send(DirectEvent::Completed(vec![result("Precise edit", 0)])).unwrap();
        });
        app.session
            .edit("Selection hole", |doc, _| {
                let mut mask = Surface::new(photocraft_color::PixelFormat::GRAY8);
                mask.fill_rect(Rect::new(5, 7, 17, 17), &[1.0]);
                mask.fill_rect(Rect::new(8, 9, 11, 12), &[0.0]);
                doc.selection = Some(mask);
                Ok(())
            })
            .unwrap();
        handle(&mut app, "yorusai.remix", &json!({"model":"ideogram-4-5-precise-edit","quality":"very_low"})).unwrap().unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.yorus_ai.error, "{}", app.ui.yorus_ai.status);
    }

    #[test]
    fn resized_source_retains_every_result_and_prevents_replacement() {
        let mut app = app(|_, sender| sender.send(DirectEvent::Completed(vec![result("Base", 0), result("Object", 1)])).unwrap());
        start(&mut app, "layers").unwrap();
        app.run("image.canvasSize", json!({"width":50,"height":40})).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.ui.yorus_ai.error);
        assert_eq!(app.yorus_ai_runtime.pending.len(), 2);
        assert!(start(&mut app, "remix").is_err());
        app.run("edit.undo", json!({})).unwrap();
        place_pending(&mut app);
        assert!(app.yorus_ai_runtime.pending.is_empty());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 3);
    }

    #[test]
    fn cancellation_discards_late_provider_result() {
        let mut app = ready_app();
        start(&mut app, "remix").unwrap();
        handle(&mut app, "yorusai.cancel", &json!({})).unwrap().unwrap();
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
        assert!(app.yorus_ai_runtime.pending.is_empty());
        assert!(!app.yorus_ai_runtime.busy());
    }

    #[test]
    fn invalid_input_and_missing_keys_never_start_provider() {
        let mut app = app(|_, _| panic!("must not call provider"));
        for params in [json!([]), json!({"model":"unknown"}), json!({"scaleFactor":99}), json!({"ratio":"0:0"}), json!({"quality":"fake"}), json!({"width":-1})]
        {
            assert!(handle(&mut app, "yorusai.run", &params).unwrap().is_err());
        }
        app.run("select.deselect", json!({})).unwrap();
        assert!(start(&mut app, "remix").is_err());
        app.yorus_ai_runtime.keys = DirectKeys::default();
        assert!(start(&mut app, "layers").is_err());
        app.ui.yorus_ai.new_canvas = true;
        app.ui.yorus_ai.width = u32::MAX;
        assert!(start(&mut app, "remix").is_err());
        assert!(!app.yorus_ai_runtime.busy());
    }

    #[test]
    fn references_are_bounded_and_keys_and_images_never_serialize_in_ui_state() {
        let mut app = ready_app();
        let bytes = png([40, 90, 160, 255]);
        for _ in 0..MAX_REFS {
            add_reference(&mut app, "C:\\private\\style.png".into(), bytes.clone()).unwrap();
        }
        assert!(add_reference(&mut app, "extra.png".into(), bytes).is_err());
        assert_eq!(app.yorus_ai_runtime.references[0].name, "style.png");
        app.yorus_ai_runtime.key_input = "test-secret-never-persist".into();
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(!saved.contains("test-secret-never-persist"));
        assert!(!saved.contains("style.png"));
        assert!(!saved.contains("iVBOR"));
        let inspected = handle(&mut app, "yorusai.inspect", &json!({})).unwrap().unwrap().to_string();
        assert!(!inspected.contains("test-secret-never-persist"));
        handle(&mut app, "yorusai.open", &json!({})).unwrap().unwrap();
        assert_eq!(app.ui.ai_sidebar.tab, crate::ai_sidebar::Tab::YorusAi);
    }

    #[test]
    fn option_matrices_and_prompt_builder_preserve_plugin_contract() {
        let keys = DirectKeys { replicate: true, byteplus: true };
        assert_eq!(provider("seedream-5.5", keys), "byteplus");
        assert_eq!(provider("seedream-5.5", DirectKeys { byteplus: false, ..keys }), "replicate", "Replicate hosts Seedream 5.0 Pro");
        assert_eq!(provider("seedream-5.5", DirectKeys::default()), "byteplus", "with no key, ask for the BytePlus one");
        assert_eq!(resolutions("seedream-5.5"), &["1K", "1.5K", "2K"]);
        assert!(!ratios("seedream-5.5").contains(&"5:4"));
        let state = State { model: "nano-banana-2.1".into(), resolution: "4K".into(), ratio: "16:9".into(), ..Default::default() };
        let [width, height] = preset_size(&state);
        assert!(width > height && width >= 3840, "{width}x{height}");
        let builder = PromptBuilder { style_reference: true, ..Default::default() };
        let prompt = make_prompt(&builder);
        assert!(prompt.contains("IMAGE B"));
        assert!(prompt.contains("Do not copy its design"));
        assert!(prompt.contains("Preserve the exact composition"));
    }

    #[test]
    fn retired_models_migrate_and_seedream_auto_keeps_the_captured_or_canvas_shape() {
        let mut old_gpt = State { model: "gpt-image-2".into(), ..Default::default() };
        normalize(&mut old_gpt);
        assert_eq!(old_gpt.model, "nano-banana-2.1", "the removed OpenAI API model falls back to the default");
        let mut old_seedream = State { model: "seedream-5-pro".into(), ratio: "3:2".into(), resolution: "2K".into(), ..Default::default() };
        normalize(&mut old_seedream);
        assert_eq!((old_seedream.model.as_str(), old_seedream.ratio.as_str(), old_seedream.resolution.as_str()), ("seedream-5.5", "3:2", "2K"));
        let mut saved = State { model: "seedream-5.5".into(), ratio: "3:2".into(), resolution: "1.5K".into(), background: "transparent".into(), ..Default::default() };
        let before = saved.clone();
        normalize(&mut saved);
        assert_eq!(saved, before, "existing valid explicit settings remain unchanged");
        for new_canvas in [false, true] {
            let mut app = app(move |request, sender| {
                assert_eq!(request.model, "seedream-5.5");
                assert_eq!(request.background, "transparent");
                assert_eq!(request.ratio, "auto");
                assert_eq!(request.input_size, if new_canvas { [31, 19] } else { [11, 17] });
                assert_eq!(request.reference.is_none(), new_canvas);
                sender.send(DirectEvent::Completed(vec![result("Auto frame", 0)])).unwrap();
            });
            app.run("select.rect", json!({"x":9,"y":5,"width":11,"height":17})).unwrap();
            handle(
                &mut app,
                "yorusai.remix",
                &json!({"model":"seedream-5.5","ratio":"auto","background":"transparent","mode":if new_canvas {"canvas"} else {"selection"},"width":31,"height":19}),
            )
            .unwrap()
            .unwrap();
            assert_eq!(preset_size(&app.ui.yorus_ai), [31, 19], "Auto must retain explicit new-canvas dimensions");
            poll(&mut app, &egui::Context::default());
            assert!(!app.ui.yorus_ai.error, "{}", app.ui.yorus_ai.status);
            let bounds = app.session.active().unwrap().doc.layers.last().unwrap().surface().unwrap().content_bounds();
            assert_eq!(bounds, if new_canvas { Rect::new(0, 0, 31, 19) } else { Rect::new(9, 5, 20, 22) });
        }
    }

    #[test]
    fn layer_and_upscale_config_do_not_inherit_incompatible_remix_settings() {
        let mut app = app(|request, sender| {
            assert_eq!(request.operation, "layers");
            assert_eq!(request.model, "seedream-5.5");
            assert_eq!(request.resolution, "2K");
            assert_eq!(request.prompt, "Separate the plants");
            assert_eq!((request.max_layers, request.text_layers), (30, true));
            sender.send(DirectEvent::Completed(vec![result("Plants", 0)])).unwrap();
        });
        app.ui.yorus_ai.model = "flux-3".into();
        app.ui.yorus_ai.resolution = "1k".into();
        handle(&mut app, "yorusai.layers", &json!({"prompt":"Separate the plants","resolution":"2K","maxLayers":30,"textLayers":true})).unwrap().unwrap();
        assert!(configure(&mut app, &json!({"maxLayers":101}), "layers").is_err());
        assert_eq!(app.ui.yorus_ai.model, "flux-3");
        assert_eq!(app.ui.yorus_ai.resolution, "1k");
        assert_eq!(app.ui.yorus_ai.prompt, "Restyle the selected image");
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.yorus_ai.error);
        app.ui.yorus_ai.model = "ideogram-4-5-precise-edit".into();
        configure(&mut app, &json!({"mode":"canvas","scaleFactor":2}), "upscale").unwrap();
        assert!(app.ui.yorus_ai.upscale_new_canvas);
    }

    #[test]
    fn native_key_callbacks_only_return_presence_and_clear_typed_secret() {
        let mut app = ready_app();
        app.services.yorus_ai_save_key = Some(Box::new(|provider, key, sender| {
            assert_eq!(provider, "byteplus");
            assert_eq!(key, "test-dummy-key");
            sender.send(Ok(DirectKeys { byteplus: true, ..Default::default() })).unwrap();
        }));
        app.yorus_ai_runtime.key_provider = "byteplus".into();
        app.yorus_ai_runtime.key_input = "test-dummy-key".into();
        save_key(&mut app, false).unwrap();
        assert!(app.yorus_ai_runtime.key_input.is_empty());
        poll(&mut app, &egui::Context::default());
        assert!(app.yorus_ai_runtime.keys.byteplus);
        app.yorus_ai_runtime.key_provider = "openai".into();
        assert!(save_key(&mut app, false).is_err(), "OpenAI API keys are no longer accepted");
        assert!(!app.yorus_ai_runtime.inspect().to_string().contains("test-dummy-key"));
        assert_eq!(app.ui.yorus_ai.status, "Provider keys updated.");
    }

    #[test]
    fn narrow_native_panel_renders_each_tool_and_key_controls_without_overflow() {
        for theme in [crate::theme::ThemeKind::StudioLight, crate::theme::ThemeKind::Studio] {
            for tab in ["remix", "upscale", "layers", "keys"] {
                let mut app = app(|_, _| panic!("rendering must not start a provider request"));
                app.ui.yorus_ai.tab = tab.into();
                app.yorus_ai_runtime.show_keys = tab == "keys";
                let ctx = egui::Context::default();
                PhotocraftApp::setup_context(&ctx, theme);
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(280.0, 620.0));
                let mut content = egui::Rect::NOTHING;
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                    panel(&mut app, ui);
                    content = ui.min_rect();
                });
                // This is a geometry-only frame; no renderer consumes its font atlas uploads.
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
                assert!(content.right() <= rect.right() + 1.0, "{theme:?}/{tab}: {content:?}");
            }
        }
    }

    #[test]
    fn live_command_specs_include_parameters_and_current_document_requirements() {
        let mut app = ready_app();
        let specs = command_specs(&app);
        assert_eq!(specs.len(), 6);
        for id in ["yorusai.run", "yorusai.remix", "yorusai.upscale", "yorusai.layers", "yorusai.inspect", "yorusai.cancel"] {
            let matching: Vec<_> = specs.iter().filter(|s| s["id"] == id).collect();
            assert_eq!(matching.len(), 1, "{id}");
            let spec = matching[0];
            assert!(spec["enabled"].as_bool().unwrap());
            assert!(spec["disabledReason"].is_null());
            assert!(spec["menu"].as_array().unwrap().is_empty());
            assert!(spec["shortcut"].is_null());
            assert_eq!(spec["needsDocument"], false);
            let params = spec["params"].as_str().unwrap();
            assert!(!params.is_empty());
            if id == "yorusai.remix" {
                assert!(params.contains("nano-banana-2.1") && params.contains("mode") && params.contains("width") && params.contains("quality"));
            }
            if id == "yorusai.upscale" {
                assert!(params.contains("scaleFactor") && params.contains("2..8"));
            }
            if id == "yorusai.layers" {
                assert!(params.contains("prompt") && params.contains("resolution") && params.contains("BytePlus") && params.contains("maxLayers"));
            }
            if id == "yorusai.remix" {
                assert!(!params.contains("gpt-image") && params.contains("seedream-5.5") && params.contains("background"));
            }
        }
        app.session = photocraft_engine::Session::new();
        let specs = command_specs(&app);
        assert_eq!(specs.iter().find(|s| s["id"] == "yorusai.layers").unwrap()["needsDocument"], true);
        app.ui.yorus_ai.new_canvas = true;
        let specs = command_specs(&app);
        let remix = specs.iter().find(|s| s["id"] == "yorusai.remix").unwrap();
        assert_eq!(remix["needsDocument"], false);
        assert_eq!(remix["enabled"], true);
    }

    #[test]
    fn codex_turn_can_inspect_metadata_but_cannot_recursively_launch_direct_generation() {
        let mut app = ready_app();
        app.services.codex_chat = Some(Box::new(|_, _, _| {}));
        app.ui.codex.chat_prompt = "Inspect the canvas".into();
        crate::codex_ui::send_chat(&mut app).unwrap();
        assert!(app.codex_runtime.busy());
        let specs = command_specs(&app);
        for spec in &specs {
            let query = spec["id"] == "yorusai.inspect" || spec["id"] == "yorusai.cancel";
            assert_eq!(spec["enabled"], query);
        }
        assert!(handle(&mut app, "yorusai.inspect", &json!({})).unwrap().is_ok());
        assert!(handle(&mut app, "yorusai.run", &json!({"operation":"remix"})).unwrap().is_err());
        assert!(!app.yorus_ai_runtime.busy());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }
}
