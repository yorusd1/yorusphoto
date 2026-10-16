//! Reproducible 24/36 MP encoded-pixel Move preview benchmark (no compositor or GPU).
//! Run release with --reps 3..5, --megapixels 24|36, --depth 8|16|32.
//! Each invocation measures one depth in one process; setup is outside the timer.

use photocraft_color::PixelFormat;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use std::time::Instant;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let option =
        |name: &str| args.iter().position(|argument| argument == name).and_then(|index| args.get(index + 1)).and_then(|value| value.parse::<u32>().ok());
    let reps = option("--reps").unwrap_or(5).clamp(3, 5);
    let height = if option("--megapixels") == Some(36) { 6000 } else { 4000 };
    let (name, format) = match option("--depth") {
        Some(16) => ("rgba16", PixelFormat::RGBA16),
        Some(32) => ("rgba32f", PixelFormat::RGBA32F),
        _ => ("rgba8", PixelFormat::RGBA8),
    };
    let bounds = Rect::new(0, 0, 6000, height);
    let mut source = Surface::new(format);
    source.fill_rect(bounds, &[0.25, 0.5, 0.75, 1.0]);
    // A solid fill shares encoded tiles. Give every source tile its own allocation so this
    // measures photo-sized memory traffic rather than repeatedly copying one cached tile.
    for coordinate in bounds.tiles() {
        let rect = coordinate.rect();
        let value = ((coordinate.tx * 37 + coordinate.ty * 91) % 251) as f32 / 250.0;
        source.write_pixel(rect.x0, rect.y0, &[value, 0.5, 0.75, 1.0]);
    }
    let warm = source.translated(7, 3, bounds);
    std::hint::black_box(warm.tile_count());
    drop(warm);
    let mut samples = Vec::new();
    let mut tiles = 0usize;
    for iteration in 0..reps {
        let start = Instant::now();
        let moved = source.translated(7 + (iteration % 3) as i32, 3 + (iteration % 2) as i32, bounds);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        tiles = std::hint::black_box(moved.tile_count());
        drop(moved);
    }
    samples.sort_by(f64::total_cmp);
    let median = samples.get(samples.len() / 2).copied().unwrap_or_default();
    let min = samples.first().copied().unwrap_or_default();
    let max = samples.last().copied().unwrap_or_default();
    println!(
        "{{\"width\":6000,\"height\":{height},\"reps\":{reps},\"format\":\"{name}\",\"median_ms\":{median:.3},\"min_ms\":{min:.3},\"max_ms\":{max:.3},\"target_tiles\":{tiles}}}"
    );
}
