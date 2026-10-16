//! Isolated large-document Navigator benchmark using the application's actual cache/update path.
//! Run in release: --reps 7 --json verification/v7-navigator-after.json.

use std::time::{Duration, Instant};

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_engine::Session;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use photocraft_testkit::perf::{RssSampler, report, row, write_report};
use photocraft_ui_egui::{PhotocraftApp, canvas, gpu_canvas};
use serde_json::json;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|v| v == name).and_then(|i| args.get(i + 1)).cloned();
    let reps = arg("--reps").and_then(|v| v.parse::<usize>().ok()).unwrap_or(7).clamp(1, 100);
    let setup = gpu_canvas::wgpu_setup();
    let rs = egui_kittest::wgpu::create_render_state(setup, Default::default());
    let adapter = rs.adapter.get_info();
    let ctx = egui::Context::default();
    let sampler = RssSampler::start(Duration::from_millis(3));
    let mut rows = Vec::new();
    for (w, h) in [(6000u32, 4000u32), (7200, 5000)] {
        let mut doc = Document::new("Navigator performance fixture", Size::new(w, h), ColorMode::Rgb, SampleType::U8);
        for i in 0..3 {
            let mut surface = Surface::new(doc.pixel_format());
            // Broad color strips and translucent layers make every output region nonempty.
            // Tiles are shared as in normal flat fills; no photo/credentials/files are needed.
            for band in 0..16u32 {
                let color = [band as f32 / 16.0, i as f32 / 3.0, 0.7, if i == 0 { 1.0 } else { 0.3 }];
                surface.fill_rect(Rect::new((band * w / 16) as i32, 0, ((band + 1) * w / 16) as i32, h as i32), &color);
            }
            doc.layers.push(Layer::new(format!("Fixture {}", i + 1), LayerContent::Raster(surface)));
        }
        let layer = doc.layers.last().map(|l| l.id).ok_or("missing fixture layer")?;
        let mut session = Session::new();
        session.open_document(doc, None);
        let mut app = PhotocraftApp::new(session, Default::default());
        app.set_wgpu(rs.clone());
        canvas::navigator_texture(&mut app, &ctx, 0).ok_or("Navigator initialization failed")?;
        let mut samples = Vec::new();
        sampler.reset();
        for i in 0..=reps {
            app.run("layer.setProps", json!({"layer": layer.0, "visible": i % 2 != 0}))?;
            let started = Instant::now();
            let texture = canvas::navigator_texture(&mut app, &ctx, 0).ok_or("Navigator update failed")?;
            std::hint::black_box(texture);
            if i > 0 {
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            // Consume texture deltas so the harness does not retain every uploaded thumbnail.
            let mut frame = ctx.run_ui(Default::default(), |_| {});
            frame.textures_delta.clear();
        }
        rows.push(row(&format!("Navigator visibility {w}x{h} (3 layers)"), &samples, sampler.peak(), None));
    }
    let result = report(
        "actual Navigator cache",
        json!({"buildProfile": if cfg!(debug_assertions) {"dev"} else {"release"}, "adapter": adapter.name, "backend": format!("{:?}", adapter.backend), "reps": reps, "fixture": "synthetic RGB8, three full-canvas layers; timer includes Navigator update only"}),
        rows,
        Some(&sampler),
    );
    if let Some(path) = arg("--json") {
        write_report(&path, &result)?;
    }
    println!("{}", serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?);
    Ok(())
}
