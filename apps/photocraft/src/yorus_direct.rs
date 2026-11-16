//! Native port of YorusAi Direct's provider contracts. API credentials stay in the OS
//! credential store; only configured flags cross the UI service boundary.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use photocraft_codecs::{ChannelLayout, DecodeOptions, EncodeOptions, Format, Image, Limits, SampleType};
use photocraft_ui_egui::{DirectBounds, DirectEvent, DirectImage, DirectKeys, DirectRequest};
use reqwest::blocking::{Client, RequestBuilder, Response, multipart};
use serde_json::{Value, json};
use std::io::Read;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::time::{Duration, Instant};

const MAX_IMAGE: usize = 32 * 1024 * 1024;
const MAX_INPUT: usize = 128 * 1024 * 1024;
const MAX_JSON: usize = 48 * 1024 * 1024;
// Match the engine's ai.apply/ai.open decode budget so paid output remains placeable.
const MAX_PIXELS: u64 = 16_777_216;
/// Seedream 5.5 Layer Separation returns a base and up to 99 layers.
const MAX_LAYERS: usize = 100;
const TIMEOUT: Duration = Duration::from_secs(30 * 60);
const NANO: &str = "google/nano-banana-2.1";
const NANO_FALLBACK: &str = "google/nano-banana-2";
/// Replicate does not host Seedream 5.5 yet; with only a Replicate key, Seedream runs 5.0 Pro there.
const SEEDREAM: &str = "bytedance/seedream-5-pro";
const IDEOGRAM: &str = "ideogram-ai/ideogram-4-5-precise-edit";
const FLUX: &str = "black-forest-labs/flux-3-image";
const CRYSTAL: &str = "philz1337x/crystal-upscaler:a69878d2abf96b6bab59327590164c9ce54993b41bbade14999c2b136079f891";
/// Seedream 5.5 on BytePlus ModelArk (ap-southeast-1), released November 2026.
const BYTEPLUS_MODEL: &str = "dola-seedream-5-5-261028";
/// Seedream 5.5 explicit sizes: total pixels and aspect-ratio bounds (BytePlus API reference).
const SEEDREAM_MIN_PIXELS: f64 = 921_600.0;
const SEEDREAM_MAX_PIXELS: f64 = 4_624_220.0;
/// Layer Separation input: total pixels from 512x512 to 6000x6000, PNG or JPEG.
const LAYER_MIN_PIXELS: u64 = 512 * 512;
const LAYER_MAX_PIXELS: u64 = 6000 * 6000;

pub fn generate(request: DirectRequest, events: Sender<DirectEvent>) {
    let failure = events.clone();
    if std::thread::Builder::new()
        .name("yorusphoto-direct".into())
        .spawn(move || {
            let result = (|| {
                validate(&request)?;
                let backend = Backend::new()?;
                let credentials = Credentials::load()?;
                backend.generate(&request, &credentials, &events)
            })();
            let event = match result {
                Ok(images) if !request.cancel.load(Ordering::Relaxed) => DirectEvent::Completed(images),
                Ok(_) => DirectEvent::Error("Cancelled.".into()),
                Err(error) => DirectEvent::Error(error),
            };
            let _ = events.send(event);
        })
        .is_err()
    {
        let _ = failure.send(DirectEvent::Error("Could not start the Direct API worker.".into()));
    }
}

pub fn key_status(reply: Sender<Result<DirectKeys, String>>) {
    let failure = reply.clone();
    if std::thread::Builder::new()
        .name("yorusphoto-direct-keys".into())
        .spawn(move || {
            let _ = reply.send(Credentials::load().map(|keys| keys.status()));
        })
        .is_err()
    {
        let _ = failure.send(Err("Could not read the operating system credential store.".into()));
    }
}

pub fn save_key(provider: String, key: String, reply: Sender<Result<DirectKeys, String>>) {
    let failure = reply.clone();
    if std::thread::Builder::new()
        .name("yorusphoto-direct-key-save".into())
        .spawn(move || {
            let result = (|| {
                provider_label(&provider)?;
                let key = key.trim();
                if !key.is_empty() {
                    if !(8..=4096).contains(&key.len()) || key.chars().any(char::is_whitespace) {
                        return Err("Enter a valid API key without spaces.".into());
                    }
                    Backend::new()?.verify_key(&provider, key)?;
                }
                store_key(&provider, key)?;
                Credentials::load().map(|keys| keys.status())
            })();
            let _ = reply.send(result);
        })
        .is_err()
    {
        let _ = failure.send(Err("Could not start the credential storage worker.".into()));
    }
}

struct Credentials {
    replicate: String,
    byteplus: String,
}

impl Credentials {
    fn load() -> Result<Self, String> {
        Ok(Self { replicate: load_key("replicate")?, byteplus: load_key("byteplus")? })
    }
    fn status(&self) -> DirectKeys {
        DirectKeys { replicate: !self.replicate.is_empty(), byteplus: !self.byteplus.is_empty() }
    }
    fn require(&self, provider: &str) -> Result<&str, String> {
        let value = match provider {
            "replicate" => &self.replicate,
            "byteplus" => &self.byteplus,
            _ => return Err("Unknown API provider.".into()),
        };
        if value.is_empty() { Err(format!("Add your {} API key first.", provider_label(provider)?)) } else { Ok(value) }
    }
}

fn provider_label(provider: &str) -> Result<&'static str, String> {
    match provider {
        "replicate" => Ok("Replicate"),
        "byteplus" => Ok("BytePlus ModelArk"),
        _ => Err("Unknown API provider.".into()),
    }
}

#[cfg(windows)]
fn credential(provider: &str) -> Result<keyring::Entry, String> {
    provider_label(provider)?;
    keyring::Entry::new("yorusphoto.direct-api", provider).map_err(|_| "Windows Credential Manager could not open the API key entry.".into())
}

#[cfg(windows)]
fn load_key(provider: &str) -> Result<String, String> {
    match credential(provider)?.get_password() {
        Ok(key) => Ok(key),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(_) => Err("Windows Credential Manager could not read the API key. No plaintext fallback is used.".into()),
    }
}

#[cfg(windows)]
fn store_key(provider: &str, key: &str) -> Result<(), String> {
    let entry = credential(provider)?;
    let result = if key.is_empty() { entry.delete_credential() } else { entry.set_password(key) };
    match result {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) if key.is_empty() => Ok(()),
        Err(_) => Err("Windows Credential Manager could not save or remove the API key. No plaintext fallback is used.".into()),
    }
}

#[cfg(not(windows))]
fn load_key(_provider: &str) -> Result<String, String> {
    Err("Direct API secure key storage currently requires Windows Credential Manager.".into())
}
#[cfg(not(windows))]
fn store_key(_provider: &str, _key: &str) -> Result<(), String> {
    Err("Direct API secure key storage currently requires Windows Credential Manager.".into())
}

struct Backend {
    client: Client,
    replicate: String,
    byteplus: String,
    poll_interval: Duration,
}

impl Backend {
    fn new() -> Result<Self, String> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(10 * 60))
            .build()
            .map_err(|_| "Could not initialize the native HTTPS client.".to_owned())?;
        Ok(Self {
            client,
            replicate: "https://api.replicate.com/v1".into(),
            byteplus: "https://ark.ap-southeast.bytepluses.com/api/v3".into(),
            poll_interval: Duration::from_millis(2500),
        })
    }

    fn verify_key(&self, provider: &str, key: &str) -> Result<(), String> {
        let url = match provider {
            "replicate" => format!("{}/account", self.replicate),
            "byteplus" => return Ok(()),
            _ => return Err("Unknown API provider.".into()),
        };
        let response = self
            .client
            .get(url)
            .bearer_auth(key)
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|_| format!("Could not verify the {} key. Check your connection.", provider_label(provider).unwrap_or("provider")))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("{} rejected key verification (HTTP {}). The key was not saved.", provider_label(provider)?, response.status().as_u16()))
        }
    }

    fn json(&self, builder: RequestBuilder, provider: &str, key: &str) -> Result<Value, String> {
        let response = builder.send().map_err(|_| format!("{provider}: HTTPS request failed or timed out. Check your connection."))?;
        let status = response.status();
        let bytes = read_response(response, MAX_JSON)?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| format!("{provider}: returned an invalid JSON response (HTTP {}).", status.as_u16()))?;
        if !status.is_success() {
            return Err(provider_error(provider, status.as_u16(), &data, key));
        }
        Ok(data)
    }

    fn generate(&self, request: &DirectRequest, keys: &Credentials, events: &Sender<DirectEvent>) -> Result<Vec<DirectImage>, String> {
        check_cancel(&request.cancel)?;
        progress(events, "Preparing Direct API request…");
        // Seedream 5.5 is on BytePlus ModelArk; Replicate only hosts Seedream 5.0 Pro.
        let seedream = request.model == "seedream-5.5";
        if request.operation == "layers" || (seedream && (!keys.byteplus.is_empty() || keys.replicate.is_empty())) {
            return self.byteplus(request, keys.require("byteplus")?, events);
        }
        if seedream && request.operation == "remix" {
            progress(events, "Seedream 5.5 needs a BytePlus key; Replicate runs Seedream 5.0 Pro instead…");
        }
        let key = keys.require("replicate")?;
        progress(events, "Uploading reference images to Replicate…");
        let mut urls = Vec::new();
        for (index, bytes) in request.reference.iter().chain(request.references.iter()).enumerate() {
            check_cancel(&request.cancel)?;
            urls.push(self.replicate_file(bytes, &format!("reference-{index}.png"), key)?);
        }
        let (model, input) = if request.operation == "upscale" {
            (CRYSTAL, json!({"image":urls.first().ok_or("Select an area to upscale first.")?,"scale_factor":request.scale_factor}))
        } else if request.model == "ideogram-4-5-precise-edit" {
            let mut input = json!({"prompt":request.prompt,"image":urls.first().ok_or("Ideogram Precise Edit requires a captured selection.")?,"quality":if ["very_low","low","medium","high"].contains(&request.quality.as_str()) { request.quality.as_str() } else { "medium" },"num_images":1});
            if let Some(mask) = &request.mask {
                input["mask"] = json!(self.replicate_file(mask, "selection-mask.png", key)?);
            }
            let count = if request.mask.is_some() { 3 } else { 4 };
            input["reference_images"] = json!(urls.iter().skip(1).take(count).collect::<Vec<_>>());
            (IDEOGRAM, input)
        } else {
            (replicate_model(&request.model)?, replicate_input(request, &urls))
        };
        let result = self.prediction(model, &input, key, &request.cancel, events);
        let url = match result {
            Err(_) if model == NANO && !request.cancel.load(Ordering::Relaxed) => {
                progress(events, "Nano Banana 2.1 failed; retrying once with Nano Banana 2…");
                self.prediction(NANO_FALLBACK, &input, key, &request.cancel, events)?
            }
            result => result?,
        };
        check_cancel(&request.cancel)?;
        progress(events, "Downloading generated image…");
        let bytes = self.download(&url)?;
        Ok(vec![DirectImage {
            bytes,
            name: if request.operation == "upscale" { "Crystal Upscale".into() } else { request.model.clone() },
            z_index: 0,
            bounds: None,
        }])
    }

    fn replicate_file(&self, bytes: &[u8], filename: &str, key: &str) -> Result<String, String> {
        let part = image_part(bytes, filename)?;
        let result = self.json(
            self.client.post(format!("{}/files", self.replicate)).bearer_auth(key).multipart(multipart::Form::new().part("content", part)),
            "Replicate",
            key,
        );
        if let Ok(data) = result
            && let Some(url) = data.pointer("/urls/get").and_then(Value::as_str)
        {
            return Ok(url.to_owned());
        }
        // The plugin also falls back to inline input if Files API upload is unavailable.
        data_url(bytes)
    }

    fn prediction(&self, model: &str, input: &Value, key: &str, cancel: &AtomicBool, events: &Sender<DirectEvent>) -> Result<String, String> {
        check_cancel(cancel)?;
        let (path, payload) = prediction_payload(model, input);
        let mut prediction = self.json(
            self.client.post(format!("{}{path}", self.replicate)).bearer_auth(key).header("Prefer", "wait=50").header("Cancel-After", "30m").json(&payload),
            "Replicate",
            key,
        )?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let id = prediction.get("id").and_then(Value::as_str).unwrap_or_default();
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                self.cancel_prediction(id, key);
                return Err(if cancel.load(Ordering::Relaxed) { "Cancelled." } else { "Replicate: generation timed out." }.into());
            }
            match prediction.get("status").and_then(Value::as_str) {
                Some("succeeded") => {
                    let output = prediction.get("output").ok_or("Replicate returned no image output.")?;
                    let output = output.as_array().and_then(|values| values.first()).unwrap_or(output);
                    return output
                        .as_str()
                        .or_else(|| output.get("url").and_then(Value::as_str))
                        .map(str::to_owned)
                        .ok_or_else(|| "Replicate returned no image URL.".into());
                }
                Some("failed" | "canceled") => return Err(provider_error("Replicate", 200, &prediction, key)),
                Some("starting" | "processing") => {}
                _ => return Err("Replicate returned an unrecognized prediction status.".into()),
            }
            if !valid_id(id) {
                return Err("Replicate returned an invalid prediction identifier.".into());
            }
            progress(
                events,
                if prediction.get("status").and_then(Value::as_str) == Some("starting") { "Queued at Replicate…" } else { "Generating at Replicate…" },
            );
            if let Err(error) = sleep_cancellable(self.poll_interval, cancel) {
                self.cancel_prediction(id, key);
                return Err(error);
            }
            prediction = match self.json(self.client.get(format!("{}/predictions/{id}", self.replicate)).bearer_auth(key), "Replicate", key) {
                Ok(prediction) => prediction,
                Err(error) => {
                    self.cancel_prediction(id, key);
                    return Err(error);
                }
            };
        }
    }

    fn cancel_prediction(&self, id: &str, key: &str) {
        if valid_id(id) {
            let _ = self.client.post(format!("{}/predictions/{id}/cancel", self.replicate)).bearer_auth(key).timeout(Duration::from_secs(5)).send();
        }
    }

    fn byteplus(&self, request: &DirectRequest, key: &str, events: &Sender<DirectEvent>) -> Result<Vec<DirectImage>, String> {
        let layers = request.operation == "layers";
        let mut images = Vec::new();
        for bytes in request.reference.iter().chain(request.references.iter()).take(if layers { 1 } else { 10 }) {
            images.push(if layers { data_url(&layer_input(bytes)?)? } else { data_url(bytes)? });
        }
        // Seedream 5.5 adds an "AI generated" mark unless told not to.
        let mut payload = json!({"model":BYTEPLUS_MODEL,"output_format":"png","response_format":"url","watermark":false});
        if !request.prompt.is_empty() {
            payload["prompt"] = json!(request.prompt);
        }
        if layers {
            // Layer Separation takes a resolution tier only, and refuses a `background`.
            payload["size"] = json!(if ["1.5K", "2K"].contains(&request.resolution.as_str()) { request.resolution.as_str() } else { "1K" });
            payload["image"] = json!(images.first().ok_or("Layer separation needs a reference image.")?);
            payload["layer_decomposition"] = json!(true);
            let most = request.max_layers.clamp(2, MAX_LAYERS as u32);
            payload["layer_decomposition_options"] = json!({"max_images":most,"text_recognition":request.text_layers});
        } else {
            payload["size"] = json!(seedream_size(&request.ratio, &request.resolution, request.input_size)?);
            payload["background"] = json!(if ["transparent", "auto"].contains(&request.background.as_str()) { request.background.as_str() } else { "opaque" });
            if !images.is_empty() {
                payload["image"] = json!(images);
            }
        }
        progress(events, if layers { "Separating layers with Seedream 5.5 on BytePlus ModelArk…" } else { "Generating with Seedream 5.5 on BytePlus ModelArk…" });
        check_cancel(&request.cancel)?;
        let data = self.json(self.client.post(format!("{}/images/generations", self.byteplus)).bearer_auth(key).json(&payload), "BytePlus", key)?;
        if data.get("error").is_some_and(|error| !error.is_null()) {
            return Err(provider_error("BytePlus", 200, &data, key));
        }
        let items = data.get("data").and_then(Value::as_array).filter(|items| !items.is_empty()).ok_or("BytePlus returned no image outputs.")?;
        if items.len() > MAX_LAYERS {
            return Err("BytePlus returned more than 100 images.".into());
        }
        for item in items {
            if item.get("error").is_some_and(|error| !error.is_null()) {
                return Err(provider_error("BytePlus", 200, item, key));
            }
        }
        let package_size = items
            .iter()
            .find(|item| item.get("z_index").and_then(Value::as_i64) == Some(0))
            .or_else(|| items.first())
            .and_then(|item| item.get("size").and_then(Value::as_str))
            .and_then(parse_size);
        let mut results = Vec::new();
        let mut total = 0usize;
        for (index, item) in items.iter().take(if layers { MAX_LAYERS } else { 1 }).enumerate() {
            check_cancel(&request.cancel)?;
            let url = item.get("url").and_then(Value::as_str).ok_or("BytePlus returned an image without a URL.")?;
            let mut bytes = self.download(url)?;
            let z_index = item.get("z_index").and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok()).unwrap_or(index as i32);
            let bounds = layers.then(|| DirectBounds {
                normalized: box_values(item.pointer("/bounding_box/normalized")),
                absolute: box_values(item.pointer("/bounding_box/absolute")),
                package_size,
            });
            if layers {
                bytes = expand_layer(&bytes, bounds.as_ref(), z_index, request.input_size)?;
            }
            total = total.saturating_add(bytes.len());
            if total > MAX_INPUT {
                return Err("The returned layer package exceeds the 128 MiB limit.".into());
            }
            results.push(DirectImage {
                bytes,
                // A text layer read with text recognition is named by its text.
                name: item
                    .pointer("/text_recognition/text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .or_else(|| item.get("name").and_then(Value::as_str))
                    .filter(|name| !name.is_empty())
                    .unwrap_or(if z_index == 0 { "Separated base" } else { "Separated layer" })
                    .chars()
                    .take(128)
                    .collect(),
                z_index,
                bounds,
            });
        }
        results.sort_by_key(|image| image.z_index);
        Ok(results)
    }

    fn download(&self, value: &str) -> Result<Vec<u8>, String> {
        let mut url = reqwest::Url::parse(value).map_err(|_| "The provider returned an invalid image address.".to_owned())?;
        for _ in 0..5 {
            validate_download_url(&url)?;
            // Never attach API credentials to provider/CDN output URLs.
            let response = self.client.get(url.clone()).send().map_err(|_| "Could not download the provider image.".to_owned())?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("The image download redirect had no location.")?;
                url = url.join(location).map_err(|_| "The image download redirect was invalid.".to_owned())?;
                continue;
            }
            if !response.status().is_success() {
                return Err(format!("Image download failed (HTTP {}).", response.status().as_u16()));
            }
            let bytes = read_response(response, MAX_IMAGE)?;
            decode_bounded(&bytes)?;
            return Ok(bytes);
        }
        Err("The image download exceeded five redirects.".into())
    }
}

fn progress(events: &Sender<DirectEvent>, message: &str) {
    let _ = events.send(DirectEvent::Status(message.into()));
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Cancelled.".into()) } else { Ok(()) }
}
fn sleep_cancellable(duration: Duration, cancel: &AtomicBool) -> Result<(), String> {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        check_cancel(cancel)?;
        std::thread::sleep(end.saturating_duration_since(Instant::now()).min(Duration::from_millis(100)));
    }
    Ok(())
}
fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
fn read_response(response: Response, limit: usize) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err("The provider response exceeds the allowed size.".into());
    }
    let mut bytes = Vec::new();
    response.take((limit + 1) as u64).read_to_end(&mut bytes).map_err(|_| "Could not read the provider response.".to_owned())?;
    if bytes.len() > limit {
        return Err("The provider response exceeds the allowed size.".into());
    }
    Ok(bytes)
}
fn provider_error(provider: &str, status: u16, data: &Value, key: &str) -> String {
    let detail = data
        .pointer("/error/message")
        .or_else(|| data.get("detail"))
        .or_else(|| data.get("error"))
        .or_else(|| data.get("title"))
        .or_else(|| data.get("message"));
    let detail = detail.map(|value| value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())).unwrap_or_else(|| format!("HTTP {status}"));
    // Authentication errors may echo a masked key fragment, so replacing only the full
    // key is insufficient. UI status is persisted: never forward these error bodies.
    let lowered = detail.to_ascii_lowercase();
    let code = data.pointer("/error/code").and_then(Value::as_str).unwrap_or_default();
    if provider == "BytePlus" && (code == "ModelNotOpen" || lowered.contains("has not activated the model")) {
        return "BytePlus: Seedream 5.5 is not activated for this account. Open the ModelArk console (region ap-southeast-1), choose Model activation and activate Seedream 5.5, then try again.".into();
    }
    let key_prefix: String = key.chars().take(8).collect();
    if matches!(status, 401 | 403)
        || ["authorization", "bearer ", "api key", "api_key", "api token", "api_token"].iter().any(|marker| lowered.contains(marker))
        || (!key_prefix.is_empty() && detail.contains(&key_prefix))
    {
        return format!("{provider}: authentication failed (HTTP {status}). Check the saved API key and account permissions.");
    }
    let detail = if key.is_empty() { detail } else { detail.replace(key, "[redacted]") };
    format!("{provider}: {}", detail.chars().filter(|c| !c.is_control() || *c == '\n').take(1200).collect::<String>())
}
fn image_type(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE {
        return Err("Reference images must be smaller than 32 MiB.".into());
    }
    match photocraft_codecs::detect(bytes) {
        Some(Format::Png) => Ok("image/png"),
        Some(Format::Jpeg) => Ok("image/jpeg"),
        Some(Format::WebP) => Ok("image/webp"),
        _ => Err("Use PNG, JPEG or WebP reference images.".into()),
    }
}
fn data_url(bytes: &[u8]) -> Result<String, String> {
    Ok(format!("data:{};base64,{}", image_type(bytes)?, BASE64.encode(bytes)))
}
fn image_part(bytes: &[u8], filename: &str) -> Result<multipart::Part, String> {
    multipart::Part::bytes(bytes.to_vec()).file_name(filename.to_owned()).mime_str(image_type(bytes)?).map_err(|_| "The image content type is invalid.".into())
}
#[cfg(test)]
fn decode_image_base64(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > MAX_IMAGE.div_ceil(3) * 4 {
        return Err("The generated image exceeds 32 MiB.".into());
    }
    let bytes = BASE64.decode(encoded).map_err(|_| "The provider returned invalid image data.".to_owned())?;
    decode_bounded(&bytes)?;
    Ok(bytes)
}
fn decode_bounded(bytes: &[u8]) -> Result<Image, String> {
    image_type(bytes)?;
    let options = DecodeOptions {
        limits: Limits { max_width: 16384, max_height: 16384, max_pixels: MAX_PIXELS, max_alloc: 512 * 1024 * 1024 },
        ..DecodeOptions::default()
    };
    let image = photocraft_codecs::decode_with(bytes, &options).map_err(|_| "The provider image is damaged or exceeds the pixel limit.".to_owned())?;
    if !image.layout().is_rgb() && !image.layout().is_gray() {
        return Err("The provider image must use RGB or grayscale color.".into());
    }
    Ok(image)
}
fn validate(request: &DirectRequest) -> Result<(), String> {
    check_cancel(&request.cancel)?;
    if !["remix", "upscale", "layers"].contains(&request.operation.as_str()) {
        return Err("Unknown Direct API operation.".into());
    }
    if request.operation == "remix" {
        replicate_model(&request.model)?;
        if request.prompt.trim().is_empty() {
            return Err("Enter a prompt first.".into());
        }
    }
    if request.prompt.len() > 128 * 1024 {
        return Err("The prompt exceeds 128 KiB.".into());
    }
    if (request.model == "seedream-5.5" || request.operation == "layers") && request.prompt.chars().count() > 4000 {
        return Err("Seedream prompts must contain 4,000 characters or fewer.".into());
    }
    if (request.operation != "remix" || request.model == "ideogram-4-5-precise-edit") && request.reference.is_none() {
        return Err("This tool requires a captured image or selected area.".into());
    }
    if request.operation == "upscale" && !(2..=8).contains(&request.scale_factor) {
        return Err("Upscale factor must be between 2 and 8.".into());
    }
    if request.operation == "upscale" {
        let width = u64::from(request.input_size[0]) * u64::from(request.scale_factor);
        let height = u64::from(request.input_size[1]) * u64::from(request.scale_factor);
        if width == 0 || height == 0 || width > 16384 || height > 16384 || width.saturating_mul(height) > MAX_PIXELS {
            return Err("This selection and upscale factor exceed the native output limit. Choose a smaller area or factor before generating.".into());
        }
    }
    if request.references.len() > 13 {
        return Err("Use at most 13 additional image references.".into());
    }
    let mut total = 0usize;
    for bytes in request.reference.iter().chain(request.references.iter()).chain(request.mask.iter()) {
        image_type(bytes)?;
        total = total.saturating_add(bytes.len());
    }
    if total > MAX_INPUT {
        return Err("Reference images exceed the 128 MiB combined limit.".into());
    }
    if request.operation == "layers" && (request.input_size.contains(&0) || u64::from(request.input_size[0]) * u64::from(request.input_size[1]) > MAX_PIXELS) {
        return Err("Layer separation input exceeds the supported canvas size.".into());
    }
    Ok(())
}
/// Seedream 5.5's explicit `WIDTHxHEIGHT`: the requested ratio (or, for Auto, the captured area or
/// new canvas), at about the tier's pixel count (1K 1024², 1.5K 1536², 2K 2048²), kept inside the
/// API's 921,600..=4,624,220 pixels and 1:16..=16:1, both sides multiples of 8.
fn seedream_size(ratio: &str, resolution: &str, input_size: [u32; 2]) -> Result<String, String> {
    let aspect = match ratio.split_once(':').map(|(w, h)| (w.parse::<f64>(), h.parse::<f64>())) {
        Some((Ok(w), Ok(h))) if w > 0.0 && h > 0.0 => w / h,
        _ if ratio == "auto" || ratio.is_empty() => {
            let [width, height] = input_size;
            if width == 0 || height == 0 {
                return Err("Seedream Auto needs a nonempty selection or canvas size.".into());
            }
            f64::from(width) / f64::from(height)
        }
        _ => return Err("Unsupported Seedream aspect ratio.".into()),
    }
    .clamp(1.0 / 16.0, 16.0);
    let edge: f64 = match resolution {
        "2K" => 2048.0,
        "1.5K" => 1536.0,
        _ => 1024.0,
    };
    let pixels = (edge * edge).clamp(SEEDREAM_MIN_PIXELS, SEEDREAM_MAX_PIXELS);
    // Round to multiples of 8, then step inside the pixel bounds the rounding may cross.
    let side = |value: f64| ((value / 8.0).round().max(1.0) * 8.0) as u32;
    let (mut width, mut height) = (side((pixels * aspect).sqrt()), side((pixels / aspect).sqrt()));
    while f64::from(width) * f64::from(height) > SEEDREAM_MAX_PIXELS {
        if width >= height { width -= 8 } else { height -= 8 }
    }
    while f64::from(width) * f64::from(height) < SEEDREAM_MIN_PIXELS {
        if width <= height { width += 8 } else { height += 8 }
    }
    Ok(format!("{width}x{height}"))
}

/// The capture as Layer Separation accepts it: a PNG of 512x512 to 6000x6000 total pixels. A
/// small selection is enlarged (the returned boxes are relative, so the layers still land on
/// the original frame); a larger one is reduced.
fn layer_input(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let source = decode_bounded(bytes)?;
    let pixels = u64::from(source.width()) * u64::from(source.height());
    if (LAYER_MIN_PIXELS..=LAYER_MAX_PIXELS).contains(&pixels) && photocraft_codecs::detect(bytes) == Some(Format::Png) {
        return Ok(bytes.to_vec());
    }
    let target = if pixels < LAYER_MIN_PIXELS { LAYER_MIN_PIXELS } else { LAYER_MAX_PIXELS.min(pixels) };
    let scale = (target as f64 / pixels.max(1) as f64).sqrt();
    let width = ((f64::from(source.width()) * scale).ceil() as u32).max(1);
    let height = ((f64::from(source.height()) * scale).ceil() as u32).max(1);
    let rgba = image::RgbaImage::from_raw(source.width(), source.height(), source.to_rgba8()).ok_or("The captured area could not be prepared for Layer Separation.")?;
    let resized = image::imageops::resize(&rgba, width, height, image::imageops::FilterType::CatmullRom);
    let output = Image::from_u8(width, height, ChannelLayout::Rgba, resized.into_raw()).map_err(|_| "The captured area could not be prepared for Layer Separation.")?;
    photocraft_codecs::encode(&output, Format::Png, &EncodeOptions { embed_metadata: false, ..Default::default() }).map_err(|_| "The captured area could not be encoded for Layer Separation.".into())
}

fn replicate_model(model: &str) -> Result<&'static str, String> {
    match model {
        "nano-banana-2.1" => Ok(NANO),
        "nano-banana-2" => Ok(NANO_FALLBACK),
        "seedream-5.5" => Ok(SEEDREAM),
        "ideogram-4-5-precise-edit" => Ok(IDEOGRAM),
        "flux-3" => Ok(FLUX),
        _ => Err("Unknown Direct API image model.".into()),
    }
}
fn prediction_payload(model: &str, input: &Value) -> (String, Value) {
    if let Some((_, version)) = model.split_once(':') {
        ("/predictions".into(), json!({"version":version,"input":input}))
    } else {
        (format!("/models/{model}/predictions"), json!({"input":input}))
    }
}
fn replicate_input(request: &DirectRequest, urls: &[String]) -> Value {
    match request.model.as_str() {
        "flux-3" => {
            json!({"prompt":request.prompt,"images":urls.iter().take(10).collect::<Vec<_>>(),"aspect_ratio":if ["auto","21:9","2:1","16:9","3:2","7:5","4:3","5:4","1:1","4:5","3:4","5:7","2:3","9:16","1:2","9:21"].contains(&request.ratio.as_str()) { request.ratio.as_str() } else { "auto" },"resolution":if ["768sq","1k","1.5k","2k","4k"].contains(&request.resolution.to_lowercase().as_str()) { request.resolution.to_lowercase() } else { "1k".into() },"output_format":"jpg","output_quality":95})
        }
        "seedream-5.5" => {
            json!({"prompt":request.prompt,"image_input":urls.iter().take(10).collect::<Vec<_>>(),"size":if request.resolution == "2K" { "2K" } else { "1K" },"aspect_ratio":if ["1:1","4:3","3:4","16:9","9:16","3:2","2:3","21:9"].contains(&request.ratio.as_str()) { request.ratio.as_str() } else if urls.is_empty() { "1:1" } else { "match_input_image" },"output_format":"png"})
        }
        _ => {
            let mut value = json!({"prompt":request.prompt,"resolution":if ["2K","4K"].contains(&request.resolution.as_str()) { request.resolution.as_str() } else { "1K" },"output_format":"jpg","google_search":true,"image_search":true});
            if !urls.is_empty() {
                value["image_input"] = json!(urls.iter().take(14).collect::<Vec<_>>());
            }
            if request.ratio != "auto" && !request.ratio.is_empty() {
                value["aspect_ratio"] = json!(request.ratio);
            } else if !urls.is_empty() {
                value["aspect_ratio"] = json!("match_input_image");
            }
            value
        }
    }
}
fn validate_download_url(url: &reqwest::Url) -> Result<(), String> {
    #[cfg(test)]
    if url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
        return Ok(());
    }
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("Provider image downloads must use HTTPS without embedded credentials.".into());
    }
    if let Some(host) = url.host_str()
        && (host.eq_ignore_ascii_case("localhost")
            || host.parse::<std::net::IpAddr>().is_ok_and(|ip| match ip {
                std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified(),
                std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local(),
            }))
    {
        return Err("The provider returned a non-public image download address.".into());
    }
    Ok(())
}
fn parse_size(value: &str) -> Option<[u32; 2]> {
    let (width, height) = value.split_once('x')?;
    let size = [width.parse().ok()?, height.parse().ok()?];
    (!size.contains(&0)).then_some(size)
}
fn box_values(value: Option<&Value>) -> Option<[f64; 4]> {
    let values = value?.as_array()?;
    let array: [f64; 4] = [values.first()?.as_f64()?, values.get(1)?.as_f64()?, values.get(2)?.as_f64()?, values.get(3)?.as_f64()?];
    (values.len() == 4 && array.iter().all(|value| value.is_finite())).then_some(array)
}

/// Normalize decomposition output to the original captured image frame. Placement can then
/// reuse the exact original capture target and irregular coverage mask for every layer.
fn expand_layer(bytes: &[u8], bounds: Option<&DirectBounds>, z_index: i32, size: [u32; 2]) -> Result<Vec<u8>, String> {
    let [width, height] = size;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("The separated layer frame is too large.".into());
    }
    let image = decode_bounded(bytes)?;
    let coordinates = if z_index == 0 {
        None
    } else {
        bounds.and_then(|bounds| {
            bounds
                .normalized
                .map(|values| values.map(|value| value.clamp(0.0, 1000.0) / 1000.0))
                .filter(|values| values[2] > values[0] && values[3] > values[1])
                .or_else(|| {
                    bounds.absolute.zip(bounds.package_size).map(|(values, package)| {
                        [
                            values[0] / f64::from(package[0]),
                            values[1] / f64::from(package[1]),
                            values[2] / f64::from(package[0]),
                            values[3] / f64::from(package[1]),
                        ]
                    })
                })
        })
    };
    let [left, top, right, bottom] = coordinates.unwrap_or([0.0, 0.0, 1.0, 1.0]);
    let left = (left.clamp(0.0, 1.0) * f64::from(width)).round() as u32;
    let top = (top.clamp(0.0, 1.0) * f64::from(height)).round() as u32;
    let right = (right.clamp(0.0, 1.0) * f64::from(width)).round() as u32;
    let bottom = (bottom.clamp(0.0, 1.0) * f64::from(height)).round() as u32;
    if right <= left || bottom <= top {
        return Err("The separated layer has an empty or reversed bounding box.".into());
    }
    let source_size = [image.width(), image.height()];
    let rectangle = [left, top, right, bottom];
    // A decomposition bounding box locates the visible part, not transparent padding
    // carried by its PNG. Some providers return a full-size transparent frame here.
    // Fitting that entire frame into the box applies its inset twice and shifts the part.
    // Base layers and artifacts without a box retain their complete image frame.
    let trim_padding = coordinates.is_some() && image.layout().has_alpha();
    let mut output = if image.sample_type() == SampleType::U16 {
        let pixels = frame_rgba(image.to_rgba16(), source_size, size, rectangle, trim_padding)?;
        Image::from_u16(width, height, ChannelLayout::Rgba, &pixels)
    } else {
        let pixels = frame_rgba(image.to_rgba8(), source_size, size, rectangle, trim_padding)?;
        Image::from_u8(width, height, ChannelLayout::Rgba, pixels)
    }
    .map_err(|_| "Could not build the separated layer image.")?;
    output.icc = if image.layout().is_gray() {
        image
            .icc
            .as_ref()
            .map(|bytes| {
                let profile = photocraft_cms::Profile::parse(bytes).map_err(|_| "The returned layer has an invalid grayscale color profile.")?;
                profile.gray_as_rgb().map(|profile| profile.to_bytes().to_vec()).ok_or("The grayscale layer profile cannot be expanded to RGB.")
            })
            .transpose()?
    } else {
        image.icc
    };
    output.meta = image.meta;
    let bytes = photocraft_codecs::encode(&output, Format::Png, &EncodeOptions::default()).map_err(|_| "Could not encode the separated layer frame.")?;
    if bytes.len() > MAX_IMAGE {
        return Err("A separated layer exceeds the 32 MiB output limit.".into());
    }
    Ok(bytes)
}

fn frame_rgba<T>(pixels: Vec<T>, source_size: [u32; 2], size: [u32; 2], rectangle: [u32; 4], trim_padding: bool) -> Result<Vec<T>, String>
where
    T: image::Primitive + Default + 'static,
    image::Rgba<T>: image::Pixel<Subpixel = T>,
{
    let [width, height] = size;
    let [left, top, right, bottom] = rectangle;
    let source =
        image::ImageBuffer::<image::Rgba<T>, Vec<T>>::from_raw(source_size[0], source_size[1], pixels).ok_or("Could not decode the separated layer pixels.")?;
    let source = if trim_padding {
        let mut left = source.width();
        let mut top = source.height();
        let mut right = 0;
        let mut bottom = 0;
        for (x, y, pixel) in source.enumerate_pixels() {
            if pixel.0.get(3).is_some_and(|alpha| *alpha != T::default()) {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x.saturating_add(1));
                bottom = bottom.max(y.saturating_add(1));
            }
        }
        if right > left && bottom > top && (left != 0 || top != 0 || right != source.width() || bottom != source.height()) {
            image::imageops::crop_imm(&source, left, top, right - left, bottom - top).to_image()
        } else {
            // Preserve an intentionally empty part rather than failing the complete package.
            source
        }
    } else {
        source
    };
    let resized = image::imageops::resize(&source, right - left, bottom - top, image::imageops::FilterType::Lanczos3);
    let count = (width as usize).checked_mul(height as usize).and_then(|pixels| pixels.checked_mul(4)).ok_or("The separated layer frame size overflowed.")?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(count).map_err(|_| "Not enough memory to frame the separated image.")?;
    pixels.resize(count, T::default());
    let mut frame = image::ImageBuffer::<image::Rgba<T>, Vec<T>>::from_raw(width, height, pixels).ok_or("Could not allocate the separated layer frame.")?;
    image::imageops::replace(&mut frame, &resized, i64::from(left), i64::from(top));
    Ok(frame.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, mpsc};

    struct Captured {
        line: String,
        headers: String,
        body: Vec<u8>,
    }
    struct Step {
        method: &'static str,
        path: &'static str,
        status: u16,
        body: Vec<u8>,
    }
    fn step(method: &'static str, path: &'static str, value: Value) -> Step {
        Step { method, path, status: 200, body: serde_json::to_vec(&value).expect("fixture JSON") }
    }
    fn fixture() -> Vec<u8> {
        let image = Image::from_u8(2, 2, ChannelLayout::Rgba, [40, 150, 200, 255].repeat(4)).expect("fixture image");
        photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default()).expect("fixture PNG")
    }
    fn request() -> DirectRequest {
        DirectRequest {
            operation: "remix".into(),
            model: "nano-banana-2.1".into(),
            prompt: "Edit the public synthetic fixture.".into(),
            ratio: "auto".into(),
            resolution: "1K".into(),
            quality: "high".into(),
            scale_factor: 2,
            reference: Some(fixture()),
            mask: None,
            references: Vec::new(),
            input_size: [2, 2],
            background: "opaque".into(),
            max_layers: 17,
            text_layers: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
    fn keys() -> Credentials {
        Credentials { replicate: "test-only-replicate".into(), byteplus: "test-only-byteplus".into() }
    }
    fn mock(steps: impl FnOnce(&str) -> Vec<Step>) -> (Backend, mpsc::Receiver<Captured>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local mock listener");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let base = format!("http://{}", listener.local_addr().expect("mock address"));
        let steps = steps(&base);
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            for step in steps {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "mock request timeout for {}", step.path);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock accept: {error}"),
                    }
                };
                // Windows accepted sockets inherit the listener's nonblocking mode.
                // Read the HTTP request synchronously, bounded by the timeout.
                socket.set_nonblocking(false).expect("blocking mock connection");
                socket.set_read_timeout(Some(Duration::from_secs(10))).expect("mock timeout");
                socket.set_write_timeout(Some(Duration::from_secs(10))).expect("mock write timeout");
                let mut reader = BufReader::new(socket.try_clone().expect("mock clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                assert!(line.starts_with(&format!("{} {} ", step.method, step.path)), "unexpected request {line}");
                let mut headers = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).expect("request header");
                    if header == "\r\n" {
                        break;
                    }
                    headers.push_str(&header);
                }
                let length = headers
                    .lines()
                    .find_map(|header| header.to_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok()))
                    .unwrap_or(0);
                assert!(length < 2 * 1024 * 1024, "oversized mock request");
                let mut body = vec![0; length];
                reader.read_exact(&mut body).expect("request body");
                sender.send(Captured { line, headers, body }).expect("captured request");
                write!(socket, "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", step.status, step.body.len())
                    .expect("mock response header");
                socket.write_all(&step.body).expect("mock response body");
            }
        });
        let mut backend = Backend::new().expect("HTTP client");
        backend.replicate = format!("{base}/v1");
        backend.byteplus = format!("{base}/v1");
        backend.poll_interval = Duration::from_millis(1);
        (backend, receiver, handle)
    }

    #[test]
    fn replicate_upload_fallback_model_retry_poll_and_public_download_contract() {
        let png = fixture();
        let (backend, audit, server) = mock(|base| {
            vec![
                Step { method: "POST", path: "/v1/files", status: 503, body: br#"{"detail":"upload unavailable"}"#.to_vec() },
                step("POST", "/v1/models/google/nano-banana-2.1/predictions", json!({"id":"first","status":"failed","error":"model unavailable"})),
                step("POST", "/v1/models/google/nano-banana-2/predictions", json!({"id":"fallback","status":"starting"})),
                step("GET", "/v1/predictions/fallback", json!({"id":"fallback","status":"succeeded","output":[format!("{base}/result.png")]})),
                Step { method: "GET", path: "/result.png", status: 200, body: png.clone() },
            ]
        });
        let (events, _) = mpsc::channel();
        let output = backend.generate(&request(), &keys(), &events).expect("mock generation");
        assert_eq!(output.len(), 1);
        assert_eq!(output.first().expect("image").bytes, png);
        server.join().expect("mock server");
        let calls: Vec<_> = audit.try_iter().collect();
        assert_eq!(calls.len(), 5);
        let primary: Value = serde_json::from_slice(&calls.get(1).expect("prediction").body).expect("prediction JSON");
        assert!(primary.pointer("/input/image_input/0").and_then(Value::as_str).expect("inline fallback").starts_with("data:image/png;base64,"));
        assert_eq!(primary.pointer("/input/aspect_ratio"), Some(&json!("match_input_image")));
        assert!(calls.get(1).expect("prediction").headers.to_lowercase().contains("prefer: wait=50"));
        assert!(calls.get(4).expect("download").line.contains("/result.png"));
        assert!(!calls.get(4).expect("download").headers.to_lowercase().contains("authorization"));
    }

    #[test]
    fn byteplus_layers_contract_expands_crops_into_original_frame_and_sorts_z_order() {
        let (backend, audit, server) = mock(|base| {
            vec![
                step(
                    "POST",
                    "/v1/images/generations",
                    json!({"error":null,"data":[{"url":format!("{base}/part.png"),"size":"2x2","z_index":1,"name":"Subject","error":null,"bounding_box":{"normalized":[250,250,750,750]}},{"url":format!("{base}/base.png"),"size":"4x4","z_index":0}]}),
                ),
                Step { method: "GET", path: "/part.png", status: 200, body: fixture() },
                Step { method: "GET", path: "/base.png", status: 200, body: fixture() },
            ]
        });
        let mut request = request();
        request.operation = "layers".into();
        request.input_size = [4, 4];
        request.resolution = "2K".into();
        let (events, _) = mpsc::channel();
        let results = backend.generate(&request, &keys(), &events).expect("layer package");
        server.join().expect("mock server");
        assert_eq!(results.len(), 2);
        assert_eq!(results.first().expect("base").z_index, 0);
        let subject = decode_bounded(&results.get(1).expect("subject").bytes).expect("framed image");
        assert_eq!(subject.dimensions(), (4, 4));
        assert_eq!(subject.get(0, 0, 3), 0.0);
        assert_eq!(subject.get(1, 1, 3), 1.0);
        assert_eq!(subject.get(3, 3, 3), 0.0);
        let body: Value = serde_json::from_slice(&audit.recv().expect("BytePlus request").body).expect("BytePlus JSON");
        assert_eq!(body.get("model"), Some(&json!(BYTEPLUS_MODEL)));
        assert_eq!(body.get("layer_decomposition"), Some(&json!(true)));
        assert_eq!(body.get("size"), Some(&json!("2K")));
        assert!(body.get("image").is_some_and(Value::is_string));
        assert_eq!(body.pointer("/layer_decomposition_options/max_images"), Some(&json!(17)));
        assert_eq!(body.get("watermark"), Some(&json!(false)));
        assert!(body.get("background").is_none(), "Layer Separation refuses a background");
        // The 2x2 capture was enlarged to the 512x512 minimum the API accepts.
        let sent = body.get("image").and_then(Value::as_str).and_then(|url| url.strip_prefix("data:image/png;base64,")).expect("PNG data URL");
        let sent = decode_bounded(&BASE64.decode(sent).expect("base64")).expect("sent PNG");
        assert!(u64::from(sent.width()) * u64::from(sent.height()) >= LAYER_MIN_PIXELS);
    }

    #[test]
    fn payloads_match_plugin_model_limits_and_crystal_pin() {
        let mut request = request();
        let urls: Vec<_> = (0..15).map(|index| format!("https://example.invalid/{index}.png")).collect();
        let nano = replicate_input(&request, &urls);
        assert_eq!(nano["image_input"].as_array().expect("Nano images").len(), 14);
        assert_eq!(nano["google_search"], true);
        assert_eq!(nano["image_search"], true);
        request.model = "seedream-5.5".into();
        request.resolution = "4K".into();
        let seedream = replicate_input(&request, &urls);
        assert_eq!(seedream["image_input"].as_array().expect("Seedream images").len(), 10);
        assert_eq!(seedream["size"], "1K");
        request.model = "flux-3".into();
        let flux = replicate_input(&request, &urls);
        assert_eq!(flux["resolution"], "4k");
        assert_eq!(flux["output_quality"], 95);
        let (path, payload) = prediction_payload(CRYSTAL, &json!({"scale_factor":8}));
        assert_eq!(path, "/predictions");
        assert_eq!(payload["version"], "a69878d2abf96b6bab59327590164c9ce54993b41bbade14999c2b136079f891");
    }

    #[test]
    fn seedream_5_5_sizes_follow_the_ratio_or_capture_within_api_bounds() {
        for (ratio, input) in [("auto", [11, 17]), ("auto", [1, 16384]), ("auto", [4000, 3]), ("16:9", [1, 1]), ("9:16", [1, 1]), ("1:1", [1, 1]), ("21:9", [5, 5])] {
            for resolution in ["1K", "1.5K", "2K"] {
                let size = seedream_size(ratio, resolution, input).expect("bounded size");
                let [width, height] = parse_size(&size).expect("WIDTHxHEIGHT");
                let pixels = f64::from(width) * f64::from(height);
                assert!((SEEDREAM_MIN_PIXELS..=SEEDREAM_MAX_PIXELS).contains(&pixels), "{ratio} {resolution}: {size}");
                assert_eq!((width % 8, height % 8), (0, 0), "{size}");
                let aspect = f64::from(width) / f64::from(height);
                assert!((1.0 / 16.0..=16.0).contains(&aspect), "{size}");
            }
        }
        assert_eq!(seedream_size("1:1", "1K", [1, 1]).expect("square"), "1024x1024");
        let [width, height] = parse_size(&seedream_size("auto", "2K", [11, 17]).expect("portrait")).expect("size");
        assert!(width < height, "Auto follows a portrait capture");
        assert!(seedream_size("auto", "1K", [0, 10]).is_err());
        assert!(seedream_size("seven", "1K", [10, 10]).is_err());
    }

    #[test]
    fn seedream_5_5_generation_sends_size_background_and_no_watermark() {
        let (backend, audit, server) = mock(|base| {
            vec![
                step("POST", "/v1/images/generations", json!({"data":[{"url":format!("{base}/out.png"),"size":"1024x1024","background":"transparent"}]})),
                Step { method: "GET", path: "/out.png", status: 200, body: fixture() },
            ]
        });
        let mut request = request();
        request.model = "seedream-5.5".into();
        request.background = "transparent".into();
        request.ratio = "1:1".into();
        let (events, _) = mpsc::channel();
        assert_eq!(backend.generate(&request, &keys(), &events).expect("Seedream 5.5 mock").len(), 1);
        server.join().expect("mock server");
        let body: Value = serde_json::from_slice(&audit.recv().expect("BytePlus request").body).expect("BytePlus JSON");
        assert_eq!(body["model"], json!(BYTEPLUS_MODEL));
        assert_eq!(body["size"], json!("1024x1024"));
        assert_eq!(body["background"], json!("transparent"));
        assert_eq!(body["watermark"], json!(false));
        assert!(body.get("layer_decomposition").is_none());
    }

    #[test]
    fn an_unactivated_seedream_model_explains_the_console_step() {
        let message = provider_error("BytePlus", 404, &json!({"error":{"code":"ModelNotOpen","message":"Your account has not activated the model"}}), "k");
        assert!(message.contains("Model activation") && message.contains("Seedream 5.5"), "{message}");
    }

    #[test]
    fn decomposition_framing_preserves_sixteen_bit_gray_tone_profile() {
        let profile = photocraft_cms::Builtin::GrayGamma22.profile();
        let source =
            Image::from_u16(2, 2, ChannelLayout::GrayA, &[12345, 65535].repeat(4)).expect("16-bit gray fixture").with_icc(Some(profile.to_bytes().to_vec()));
        let bytes = photocraft_codecs::encode(&source, Format::Png, &EncodeOptions::default()).expect("gray fixture PNG");
        let bounds = DirectBounds { normalized: Some([0.0, 0.0, 0.0, 0.0]), absolute: Some([1.0, 1.0, 3.0, 3.0]), package_size: Some([4, 4]) };
        let framed = expand_layer(&bytes, Some(&bounds), 1, [4, 4]).expect("framed gray layer");
        let image = decode_bounded(&framed).expect("decoded framed layer");
        assert_eq!(image.sample_type(), SampleType::U16);
        assert_eq!(image.dimensions(), (4, 4));
        assert_eq!(image.get(0, 0, 3), 0.0);
        assert!((image.get(1, 1, 0) - 12345.0 / 65535.0).abs() < 1.0 / 65535.0);
        let profile = photocraft_cms::Profile::parse(image.icc.as_deref().expect("expanded ICC")).expect("RGB ICC profile");
        assert_eq!(profile.color_space, photocraft_cms::ColorSpace::Rgb);
    }

    fn padded_part(internal_hole: bool) -> (Vec<u8>, Vec<u8>) {
        let mut full = vec![0; 8 * 6 * 4];
        let mut cropped = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let color = if internal_hole && (x, y) == (1, 1) { [0, 0, 0, 0] } else { [20 + x * 50, 30 + y * 40, 80, 255] };
                let offset = ((y as usize + 1) * 8 + x as usize + 2) * 4;
                full[offset..offset + 4].copy_from_slice(&color);
                cropped.extend(color);
            }
        }
        let png = |width, height, pixels| {
            let image = Image::from_u8(width, height, ChannelLayout::Rgba, pixels).expect("padded image fixture");
            photocraft_codecs::encode(&image, Format::Png, &Default::default()).expect("padded PNG fixture")
        };
        (png(8, 6, full), png(4, 4, cropped))
    }

    #[test]
    fn decomposition_full_frame_padding_does_not_apply_the_provider_offset_twice() {
        let (padded, cropped) = padded_part(true);
        let bounds = DirectBounds { absolute: Some([2.0, 1.0, 6.0, 5.0]), package_size: Some([8, 6]), normalized: None };
        let full = expand_layer(&padded, Some(&bounds), 1, [8, 6]).expect("full-frame artifact");
        let trimmed = expand_layer(&cropped, Some(&bounds), 1, [8, 6]).expect("cropped artifact");
        let full = decode_bounded(&full).expect("full-frame result");
        let trimmed = decode_bounded(&trimmed).expect("cropped result");
        assert_eq!(full.to_rgba8(), trimmed.to_rgba8(), "provider crop and padded PNG must have identical placement");
        assert_eq!(full.to_rgba8(), decode_bounded(&padded).expect("original padded fixture").to_rgba8());
        assert_eq!(full.get(2, 1, 3), 1.0);
        assert_eq!(full.get(5, 4, 3), 1.0);
        assert_eq!(full.get(3, 2, 3), 0.0, "internal transparent holes are preserved");
        assert_eq!(full.get(1, 1, 3), 0.0);
        assert_eq!(full.get(6, 4, 3), 0.0);
    }

    #[test]
    fn decomposition_padded_parts_fill_odd_nonuniform_scaled_boxes_exactly() {
        // An opaque foreground isolates geometry: a nearby transparent hole legitimately
        // lowers corner alpha through the Lanczos kernel, independently of box placement.
        let (padded, _) = padded_part(false);
        let bounds = DirectBounds { absolute: Some([6.0, 4.0, 20.0, 14.0]), package_size: Some([26, 18]), normalized: None };
        let bytes = expand_layer(&padded, Some(&bounds), 2, [13, 9]).expect("odd scaled bounds");
        let image = decode_bounded(&bytes).expect("odd-sized frame");
        assert_eq!(image.dimensions(), (13, 9));
        for y in 0..9 {
            for x in 0..13 {
                let expected = if (3..10).contains(&x) && (2..7).contains(&y) { 1.0 } else { 0.0 };
                assert_eq!(image.get(x, y, 3), expected, "visible foreground must fill precisely its provider box at ({x},{y})");
            }
        }
    }

    #[test]
    fn decomposition_base_and_unpositioned_artifacts_keep_intentional_padding() {
        let (padded, _) = padded_part(true);
        let bounds = DirectBounds { normalized: Some([250.0, 1000.0 / 6.0, 750.0, 5000.0 / 6.0]), ..Default::default() };
        let original = decode_bounded(&padded).expect("original frame").to_rgba8();
        for (bounds, z_index) in [(Some(&bounds), 0), (None, 1)] {
            let bytes = expand_layer(&padded, bounds, z_index, [8, 6]).expect("full image frame");
            assert_eq!(decode_bounded(&bytes).expect("decoded frame").to_rgba8(), original);
        }
    }

    #[test]
    fn decomposition_padding_crop_keeps_sixteen_bit_gray_profile_and_alpha() {
        let mut values = vec![0; 4 * 4 * 2];
        for y in 1..3 {
            for x in 1..3 {
                let offset = (y * 4 + x) * 2;
                values[offset..offset + 2].copy_from_slice(&[12345, 65535]);
            }
        }
        let image = Image::from_u16(4, 4, ChannelLayout::GrayA, &values)
            .expect("gray padding fixture")
            .with_icc(Some(photocraft_cms::Builtin::GrayGamma22.profile().to_bytes().to_vec()));
        let bytes = photocraft_codecs::encode(&image, Format::Png, &Default::default()).expect("gray padded PNG");
        let bounds = DirectBounds { normalized: Some([250.0, 250.0, 750.0, 750.0]), ..Default::default() };
        let output = expand_layer(&bytes, Some(&bounds), 1, [4, 4]).expect("gray padded placement");
        let output = decode_bounded(&output).expect("gray output");
        assert_eq!(output.sample_type(), SampleType::U16);
        for (x, y) in [(1, 1), (2, 2)] {
            assert!((output.get(x, y, 0) - 12345.0 / 65535.0).abs() < 1.0 / 65535.0);
            assert_eq!(output.get(x, y, 3), 1.0);
        }
        assert_eq!(output.get(0, 0, 3), 0.0);
        let profile = photocraft_cms::Profile::parse(output.icc.as_deref().expect("RGB profile")).expect("valid profile");
        assert_eq!(profile.color_space, photocraft_cms::ColorSpace::Rgb);
    }

    #[test]
    fn decomposition_padding_crop_preserves_even_the_faintest_eight_and_sixteen_bit_fringe() {
        let mut eight = vec![0u8; 6 * 6 * 4];
        let mut sixteen = vec![0u16; 6 * 6 * 4];
        for y in 1..5 {
            for x in 1..5 {
                let offset = (y * 6 + x) * 4;
                let edge = x == 1 || x == 4 || y == 1 || y == 4;
                eight[offset..offset + 4].copy_from_slice(&[40, 80, 160, if edge { 1 } else { 255 }]);
                sixteen[offset..offset + 4].copy_from_slice(&[12345, 23456, 34567, if edge { 1 } else { 65535 }]);
            }
        }
        let rectangle = [1, 1, 5, 5];
        assert_eq!(frame_rgba(eight.clone(), [6, 6], [6, 6], rectangle, true).expect("8-bit feathered fringe"), eight);
        assert_eq!(frame_rgba(sixteen.clone(), [6, 6], [6, 6], rectangle, true).expect("16-bit feathered fringe"), sixteen);
    }

    #[test]
    fn cancellation_bad_inputs_and_secret_errors_fail_without_network() {
        let mut request = request();
        request.cancel.store(true, Ordering::Relaxed);
        assert_eq!(validate(&request), Err("Cancelled.".into()));
        request.cancel.store(false, Ordering::Relaxed);
        request.model = "unknown".into();
        assert!(validate(&request).is_err());
        request.model = "ideogram-4-5-precise-edit".into();
        request.reference = None;
        assert!(validate(&request).is_err());
        request.reference = Some(fixture());
        request.operation = "upscale".into();
        request.input_size = [1024, 1024];
        request.scale_factor = 8;
        assert!(validate(&request).is_err_and(|message| message.contains("native output limit")));
        let error = provider_error("Test", 401, &json!({"error":{"message":"Rejected test-only-secret"}}), "test-only-secret");
        assert!(!error.contains("test-only-secret"));
        assert!(error.contains("authentication failed"));
        let masked = provider_error(
            "Replicate",
            400,
            &json!({"error":{"message":"Incorrect API key provided: sk-proj-123********abcd; Authorization: Bearer masked-token"}}),
            "sk-proj-1234567890abcd",
        );
        assert!(!masked.contains("sk-proj"));
        assert!(!masked.contains("masked-token"));
        assert!(!masked.contains("abcd"));
        assert!(validate_download_url(&reqwest::Url::parse("https://user:password@example.com/image.png").expect("URL")).is_err());
        assert!(validate_download_url(&reqwest::Url::parse("file:///private/image.png").expect("URL")).is_err());
        assert!(decode_image_base64("invalid").is_err());
    }

    #[test]
    fn replicate_stop_during_poll_wait_cancels_known_remote_prediction() {
        let (mut backend, audit, server) = mock(|_| {
            vec![
                step("POST", "/v1/models/google/nano-banana-2.1/predictions", json!({"id":"cancel-me","status":"starting"})),
                step("POST", "/v1/predictions/cancel-me/cancel", json!({"id":"cancel-me","status":"canceled"})),
            ]
        });
        backend.poll_interval = Duration::from_secs(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let observer_cancel = cancel.clone();
        let (events, receiver) = mpsc::channel();
        let observer = std::thread::spawn(move || {
            let event = receiver.recv_timeout(Duration::from_secs(5)).expect("queued event");
            assert!(matches!(event, DirectEvent::Status(_)));
            observer_cancel.store(true, Ordering::Relaxed);
        });
        let result = backend.prediction(NANO, &json!({"prompt":"Synthetic cancellation fixture"}), "test-only-replicate", &cancel, &events);
        assert_eq!(result, Err("Cancelled.".into()));
        observer.join().expect("cancellation observer");
        server.join().expect("mock server");
        assert_eq!(audit.try_iter().count(), 2);
    }
}
