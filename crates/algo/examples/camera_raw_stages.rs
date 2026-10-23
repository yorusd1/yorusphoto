//! Per-stage Camera Raw develop timing on the dialog's preview proxy (900 × 600 by default):
//! what one slider move costs while dragging.
//! `cargo run --release -p photocraft-algo --example camera_raw_stages [width height]`
use std::time::Instant;

use photocraft_algo::camera_raw::{CameraRaw, Wheel, develop};

fn image(w: usize, h: usize) -> Vec<[f32; 4]> {
    // A photo-like mix: a wide tonal ramp, a bright "sky" band, dark "foliage" texture.
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let tex = (((x * 7 + y * 13) % 17) as f32 / 17.0 - 0.5) * 0.08;
            let base = if fy < 0.3 { 0.75 + 0.2 * fx } else { 0.05 + 0.6 * fx * (1.0 - fy) };
            let v = (base + tex).clamp(0.0, 1.0);
            [v, (v * 0.9 + 0.05 * fy).min(1.0), (v * 0.7 + 0.2 * (1.0 - fy)).min(1.0), 1.0]
        })
        .collect()
}

fn median(name: &str, reps: usize, mut f: impl FnMut()) {
    let mut t: Vec<f64> = (0..reps)
        .map(|_| {
            let t0 = Instant::now();
            f();
            t0.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(f64::total_cmp);
    println!("{name:<34} {:>8.2} ms (min {:.2})", t[t.len() / 2], t[0]);
}

fn main() {
    let args: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (w, h) = (args.first().copied().unwrap_or(900), args.get(1).copied().unwrap_or(600));
    let px = image(w, h);
    println!("{w} x {h}");
    let mut hsl = [0.0; 8];
    hsl[2] = 30.0;
    let cases: Vec<(&str, CameraRaw)> = vec![
        ("exposure", CameraRaw { exposure: 0.5, ..Default::default() }),
        ("temperature", CameraRaw { temperature: 20.0, ..Default::default() }),
        ("contrast", CameraRaw { contrast: 30.0, ..Default::default() }),
        ("highlights", CameraRaw { highlights: -60.0, ..Default::default() }),
        ("shadows", CameraRaw { shadows: 60.0, ..Default::default() }),
        ("whites", CameraRaw { whites: 30.0, ..Default::default() }),
        ("blacks", CameraRaw { blacks: -30.0, ..Default::default() }),
        ("texture", CameraRaw { texture: 40.0, ..Default::default() }),
        ("clarity", CameraRaw { clarity: 40.0, ..Default::default() }),
        ("dehaze", CameraRaw { dehaze: 40.0, ..Default::default() }),
        ("vibrance", CameraRaw { vibrance: 40.0, ..Default::default() }),
        ("hsl", CameraRaw { hsl_sat: hsl, ..Default::default() }),
        ("grading", CameraRaw { grade_shadows: Wheel { hue: 220.0, sat: 30.0, lum: 10.0 }, ..Default::default() }),
        ("point curve", CameraRaw { point_curve: vec![[0.0, 10.0], [128.0, 140.0], [255.0, 250.0]], ..Default::default() }),
        ("sharpen", CameraRaw { sharpen_amount: 40.0, sharpen_masking: 30.0, ..Default::default() }),
        ("noise reduction", CameraRaw { noise_luminance: 30.0, noise_color: 25.0, ..Default::default() }),
        ("vignette", CameraRaw { vignette_amount: -30.0, ..Default::default() }),
        ("grain", CameraRaw { grain_amount: 30.0, ..Default::default() }),
        (
            "typical edit (all light + color)",
            CameraRaw {
                exposure: 0.3,
                temperature: 10.0,
                contrast: 15.0,
                highlights: -50.0,
                shadows: 40.0,
                whites: 10.0,
                blacks: -10.0,
                clarity: 15.0,
                vibrance: 20.0,
                hsl_sat: hsl,
                sharpen_amount: 40.0,
                ..Default::default()
            },
        ),
    ];
    for (name, p) in &cases {
        median(name, 7, || {
            let mut a = px.clone();
            develop(&mut a, w, h, p, false);
            std::hint::black_box(&a);
        });
    }
}
