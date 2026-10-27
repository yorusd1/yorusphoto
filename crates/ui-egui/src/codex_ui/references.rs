//! Transient, bounded attachments for the Images tab; never serialized into chat history.

use super::{PhotocraftApp, status};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use photocraft_codecs::{DecodeOptions, EncodeOptions, Format, Limits};
use serde_json::{Value, json};
use std::sync::Arc;

const MAX_REFERENCES: usize = 4;
const MAX_BYTES: usize = 32 << 20;

pub(super) struct Reference {
    name: String,
    pub(super) png: Arc<[u8]>,
    size: [u32; 2],
    preview: egui::ColorImage,
    texture: Option<egui::TextureHandle>,
}

fn editable(app: &PhotocraftApp) -> bool {
    !app.codex_runtime.busy() && app.codex_runtime.login_rx.is_none() && !app.yorus_ai_runtime.busy()
}

fn add(app: &mut PhotocraftApp, name: String, bytes: Vec<u8>) -> Result<Value, String> {
    if !editable(app) {
        return Err("Wait for the current AI operation before changing references.".into());
    }
    if app.codex_runtime.references.len() >= MAX_REFERENCES {
        return Err("Use up to four uploaded references, plus the selected canvas region.".into());
    }
    let used = app.codex_runtime.references.iter().map(|image| image.png.len()).sum::<usize>();
    if bytes.len() > MAX_BYTES.saturating_sub(used) {
        return Err("Uploaded references together must fit within 32 MiB.".into());
    }
    let format = photocraft_codecs::detect(&bytes).ok_or("Choose a PNG, JPEG or WebP image.")?;
    if !matches!(format, Format::Png | Format::Jpeg | Format::WebP) {
        return Err("Choose a PNG, JPEG or WebP image.".into());
    }
    let image = photocraft_codecs::decode_with(
        &bytes,
        &DecodeOptions { limits: Limits { max_width: 16_384, max_height: 16_384, max_pixels: 16_777_216, max_alloc: 128 << 20 }, ..Default::default() },
    )
    .map_err(|error| format!("Could not read the reference image: {error}"))?;
    let size = [image.width(), image.height()];
    // Keep original PNG bytes and preserve codec depth/profile when converting JPEG/WebP.
    // Display-only thumbnails are small RGBA8 textures, never full-size GPU uploads.
    let png = if format == Format::Png {
        bytes
    } else {
        photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default()).map_err(|error| error.to_string())?
    };
    if png.len() > MAX_BYTES.saturating_sub(used) {
        return Err("Decoded reference PNGs together must fit within 32 MiB. Use a smaller image.".into());
    }
    let rgba = image.to_rgba8();
    let scale = 56.0 / f64::from(size[0].max(size[1]).max(1));
    let width = (f64::from(size[0]) * scale).round().max(1.0) as usize;
    let height = (f64::from(size[1]) * scale).round().max(1.0) as usize;
    let mut pixels = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4));
    for y in 0..height {
        for x in 0..width {
            let sx = x.saturating_mul(size[0] as usize) / width;
            let sy = y.saturating_mul(size[1] as usize) / height;
            let at = sy.saturating_mul(size[0] as usize).saturating_add(sx).saturating_mul(4);
            pixels.extend_from_slice(rgba.get(at..at.saturating_add(4)).unwrap_or(&[0, 0, 0, 0]));
        }
    }
    let name = name.rsplit(['/', '\\']).next().unwrap_or("Reference").chars().filter(|c| !c.is_control()).take(120).collect();
    app.codex_runtime.references.push(Reference {
        name,
        png: png.into(),
        size,
        preview: egui::ColorImage::from_rgba_unmultiplied([width, height], &pixels),
        texture: None,
    });
    Ok(inspect(app))
}

fn pick(app: &mut PhotocraftApp) -> Result<Value, String> {
    if !editable(app) || app.codex_runtime.references.len() >= MAX_REFERENCES {
        return Err("Finish the current task or remove a reference before adding another.".into());
    }
    app.ask_file(
        crate::FileDialogRequest::Open {
            multiple: false,
            initial_dir: None,
            extensions: Some(["png", "jpg", "jpeg", "webp"].into_iter().map(str::to_owned).collect()),
        },
        |app, answer| {
            let (name, bytes) = match answer {
                crate::FileDialogAnswer::Contents(name, bytes) => (name, bytes),
                crate::FileDialogAnswer::ContentsMany(files) => files.into_iter().next().ok_or(crate::file_dialog::CANCELLED)?,
                crate::FileDialogAnswer::Paths(paths) => {
                    let name = paths.into_iter().next().ok_or(crate::file_dialog::CANCELLED)?;
                    let bytes = read_reference_file(&name)?;
                    (name, bytes)
                }
                _ => return Err("Choose an image file.".into()),
            };
            add(app, name, bytes)
        },
    )
}

fn read_reference_file(name: &str) -> Result<Vec<u8>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Read as _;
        let file = std::fs::File::open(name).map_err(|_| "Could not open the chosen reference image.")?;
        let metadata = file.metadata().map_err(|_| "Could not inspect the chosen reference image.")?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
            return Err("Choose a reference image no larger than 32 MiB.".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|_| "Could not read the chosen reference image.")?;
        if bytes.len() > MAX_BYTES {
            return Err("The reference image grew beyond the 32 MiB limit.".into());
        }
        Ok(bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        Err("Use the browser image picker to upload reference bytes.".into())
    }
}

pub(super) fn inspect(app: &PhotocraftApp) -> Value {
    json!({"count":app.codex_runtime.references.len(),"limit":MAX_REFERENCES,
        "images":app.codex_runtime.references.iter().map(|r| json!({"width":r.size[0],"height":r.size[1]})).collect::<Vec<_>>()})
}

pub(super) fn specs(app: &PhotocraftApp) -> Vec<Value> {
    [
        ("codex.references.pick", "Attach a Codex image reference", "{}; opens the user's image picker"),
        ("codex.references.add", "Attach reference image bytes", "{image:base64,name?:string}; PNG/JPEG/WebP, four references, 32MiB combined"),
        ("codex.references.remove", "Remove a Codex image reference", "{index:integer}; zero-based attachment index"),
        ("codex.references.clear", "Clear Codex image references", "{}"),
    ]
    .into_iter()
    .map(|(id, label, params)| {
        json!({"id":id,"label":label,"params":params,"menu":[],"shortcut":null,
        "enabled":editable(app),"disabledReason":if editable(app) {Value::Null} else {json!("Wait for the AI operation")},"needsDocument":false})
    })
    .collect()
}

pub(super) fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !id.starts_with("codex.references.") {
        return None;
    }
    Some((|| {
        if !editable(app) || !params.is_object() {
            return Err("Reference controls need object parameters and an idle AI workspace.".into());
        }
        match id {
            "codex.references.pick" => pick(app),
            "codex.references.add" => {
                let encoded = params
                    .get("image")
                    .and_then(Value::as_str)
                    .filter(|s| s.len() <= (MAX_BYTES / 3 + 1) * 4)
                    .ok_or("Provide an image as bounded base64 data.")?;
                let name = match params.get("name") {
                    None => "Reference",
                    Some(value) => value.as_str().filter(|s| s.len() <= 4096).ok_or("Reference name must be a string within 4096 bytes.")?,
                };
                let bytes = STANDARD.decode(encoded).map_err(|_| "Invalid base64 reference image.")?;
                add(app, name.into(), bytes)
            }
            "codex.references.remove" => {
                let index = params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < app.codex_runtime.references.len())
                    .ok_or("Choose an existing reference index.")?;
                app.codex_runtime.references.remove(index);
                Ok(inspect(app))
            }
            "codex.references.clear" => {
                app.codex_runtime.references.clear();
                Ok(inspect(app))
            }
            _ => Err("Unknown image reference command.".into()),
        }
    })())
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let enabled = editable(app);
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        ui.strong("Reference images");
        ui.weak(format!("{}/{}", app.codex_runtime.references.len(), MAX_REFERENCES));
        if ui.add_enabled(enabled && !app.file_dialog_open() && app.codex_runtime.references.len() < MAX_REFERENCES, egui::Button::new("Add image…")).clicked()
            && let Err(error) = pick(app)
        {
            status(app, error, true);
        }
        if ui.add_enabled(enabled && !app.codex_runtime.references.is_empty(), egui::Button::new("Clear")).clicked() {
            app.codex_runtime.references.clear();
        }
    });
    for (index, reference) in app.codex_runtime.references.iter_mut().enumerate() {
        ui.push_id(("codex-reference", index), |ui| {
            ui.horizontal(|ui| {
                let texture = reference
                    .texture
                    .get_or_insert_with(|| ui.ctx().load_texture(format!("codex-reference-{index}"), reference.preview.clone(), egui::TextureOptions::LINEAR));
                ui.image((texture.id(), texture.size_vec2()));
                ui.vertical(|ui| {
                    crate::widgets::elided_label(ui, &reference.name, (ui.available_width() - 30.0).max(32.0));
                    ui.small(format!("{} × {} px", reference.size[0], reference.size[1]));
                });
                if ui.add_enabled(enabled, egui::Button::new("×")).on_hover_text("Remove reference").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        app.codex_runtime.references.remove(index);
    }
    ui.small("PNG, JPEG or WebP. Added alongside the selection; kept until removed or the app closes.");
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, codex_ui};
    use photocraft_codecs::{ChannelLayout, Image, SampleType};
    use std::{cell::RefCell, rc::Rc};

    fn fixture(format: Format, sample: SampleType) -> Vec<u8> {
        let image = Image::new(8, 6, ChannelLayout::Rgb, sample).unwrap();
        photocraft_codecs::encode(&image, format, &EncodeOptions::default()).unwrap()
    }

    fn app() -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), Services::default())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn oversized_picker_file_is_rejected_before_reading_its_contents() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("yorus-reference-limit-{}-{unique}.png", std::process::id()));
        let file = std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
        file.set_len((MAX_BYTES + 1) as u64).unwrap();
        drop(file);
        let result = read_reference_file(path.to_str().unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(result.unwrap_err().contains("no larger than 32 MiB"));
    }

    #[test]
    fn png_depth_and_bytes_survive_and_jpeg_webp_are_normalized_without_opening_documents() {
        let mut app = app();
        let png = fixture(Format::Png, SampleType::U16);
        add(&mut app, r"C:\private\Reference.png".into(), png.clone()).unwrap();
        let reference = app.codex_runtime.references.first().unwrap();
        assert_eq!(reference.png.as_ref(), png);
        assert_eq!(reference.name, "Reference.png");
        assert_eq!(photocraft_codecs::decode(&reference.png).unwrap().sample_type(), SampleType::U16);
        for format in [Format::Jpeg, Format::WebP] {
            add(&mut app, "Reference".into(), fixture(format, SampleType::U8)).unwrap();
            let reference = app.codex_runtime.references.last().unwrap();
            assert_eq!(photocraft_codecs::detect(&reference.png), Some(Format::Png));
            assert_eq!(reference.size, [8, 6]);
            assert!(reference.preview.size.iter().all(|n| *n <= 56));
        }
        assert!(app.session.documents().is_empty());
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(!saved.contains("Reference.png") && !saved.contains("private"));
        assert!(app.ui.codex.messages.is_empty());
    }

    #[test]
    fn corrupt_oversized_or_excess_attachments_and_invalid_removal_do_not_mutate_existing_images() {
        let mut app = app();
        let png = fixture(Format::Png, SampleType::U8);
        add(&mut app, "keep".into(), png.clone()).unwrap();
        assert!(add(&mut app, "bad".into(), b"not an image".to_vec()).is_err());
        assert!(add(&mut app, "too large".into(), vec![0; MAX_BYTES + 1]).is_err());
        assert!(menu(&mut app, "codex.references.remove", &json!({"index":u64::MAX})).unwrap().is_err());
        assert_eq!(app.codex_runtime.references.len(), 1);
        assert_eq!(app.codex_runtime.references.first().unwrap().png.as_ref(), png);
        for _ in 1..MAX_REFERENCES {
            add(&mut app, "valid".into(), png.clone()).unwrap();
        }
        assert!(add(&mut app, "fifth".into(), png).is_err());
        menu(&mut app, "codex.references.remove", &json!({"index":1})).unwrap().unwrap();
        assert_eq!(app.codex_runtime.references.len(), 3);
        menu(&mut app, "codex.references.clear", &json!({})).unwrap().unwrap();
        assert!(app.codex_runtime.references.is_empty());
    }

    #[test]
    fn native_file_picker_adds_an_attachment_without_opening_it_as_a_canvas() {
        let png = fixture(Format::Png, SampleType::U8);
        let (dialog, requests) = crate::file_dialog::fake(vec![Some(crate::FileDialogAnswer::Contents("chosen.png".into(), png))]);
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services { file_dialog: Some(dialog), ..Default::default() });
        pick(&mut app).unwrap();
        let context = egui::Context::default();
        app.poll_file_dialog(&context, None);
        app.poll_file_dialog(&context, None);
        assert_eq!(app.codex_runtime.references.len(), 1);
        assert!(app.session.documents().is_empty());
        assert!(
            matches!(requests.borrow().first(), Some(crate::FileDialogRequest::Open { extensions:Some(extensions), .. }) if extensions.contains(&"webp".into()))
        );
    }

    #[test]
    fn uploaded_references_reach_the_worker_and_selected_area_placement_remains_captured() {
        let received = Rc::new(RefCell::new(Vec::new()));
        let sink = received.clone();
        let png = fixture(Format::Png, SampleType::U8);
        let result = png.clone();
        let services = Services {
            generate_image: Some(Box::new(move |request, reply| {
                assert!(request.reference.is_some());
                *sink.borrow_mut() = request.references;
                reply.send(Ok(result.clone())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.run("select.rect", json!({"x":5,"y":7,"width":12,"height":10})).unwrap();
        add(&mut app, "style.png".into(), png).unwrap();
        let shared = app.codex_runtime.references.first().unwrap().png.clone();
        app.ui.codex.prompt = "Use the attached style in the selection".into();
        codex_ui::generate(&mut app).unwrap();
        assert!(Arc::ptr_eq(received.borrow().first().unwrap(), &shared));
        assert!(menu(&mut app, "codex.references.clear", &json!({})).unwrap().is_err());
        app.run("select.rect", json!({"x":25,"y":20,"width":2,"height":2})).unwrap();
        codex_ui::poll(&mut app, &egui::Context::default());
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        let document = app.session.active().unwrap();
        assert_eq!(document.doc.layers.len(), 2);
        let layer = document.doc.layer(document.active_layer.unwrap()).unwrap();
        assert_eq!(layer.surface().unwrap().content_bounds(), photocraft_geom::Rect::new(5, 7, 17, 17));
        assert_eq!(document.doc.selection.as_ref().unwrap().content_bounds(), photocraft_geom::Rect::new(25, 20, 27, 22));
        assert_eq!(app.codex_runtime.references.len(), 1);
    }

    #[test]
    fn uploaded_reference_without_any_canvas_generates_an_independent_new_canvas() {
        let png = fixture(Format::Png, SampleType::U8);
        let result = png.clone();
        let services = Services {
            generate_image: Some(Box::new(move |request, reply| {
                assert!(request.reference.is_none());
                assert_eq!(request.references.len(), 1);
                reply.send(Ok(result.clone())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        add(&mut app, "subject.png".into(), png).unwrap();
        app.ui.codex.entire_image = true;
        app.ui.codex.width = 128;
        app.ui.codex.height = 96;
        app.ui.codex.prompt = "Create an image using the uploaded subject".into();
        codex_ui::generate(&mut app).unwrap();
        codex_ui::poll(&mut app, &egui::Context::default());
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        assert_eq!(app.session.documents().len(), 1);
        let document = app.session.active().unwrap();
        assert_eq!((document.doc.size.width, document.doc.size.height), (128, 96));
    }
}
