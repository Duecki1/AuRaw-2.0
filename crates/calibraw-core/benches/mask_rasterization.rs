use calibraw_core::pipeline::{rasterize_brush_dabs, BrushDab};
use std::hint::black_box;
use std::time::Instant;

fn dabs(count: usize, opacity: f32) -> Vec<BrushDab> {
    (0..count)
        .map(|index| {
            let x = 0.08 + 0.84 * (index % 32) as f32 / 31.0;
            let y = 0.08 + 0.84 * (index / 32) as f32 / 7.0;
            BrushDab {
                center: [x, y],
                opacity,
                size: 0.024,
                feather: 0.55,
            }
        })
        .collect()
}

fn measure(label: &str, width: u32, height: u32, dabs: &[BrushDab]) {
    const ITERATIONS: usize = 4;
    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..ITERATIONS {
        let pixels = rasterize_brush_dabs(width, height, width, height, black_box(dabs));
        checksum = checksum.wrapping_add(
            pixels
                .iter()
                .step_by((pixels.len() / 31).max(1))
                .map(|pixel| u64::from(*pixel))
                .sum(),
        );
        black_box(pixels);
    }
    let elapsed = started.elapsed();
    let megapixels = (width as f64 * height as f64 * ITERATIONS as f64) / 1_000_000.0;
    println!(
        "{label}: {megapixels:.1} MP in {:.3}s ({:.1} MP/s), checksum {checksum}",
        elapsed.as_secs_f64(),
        megapixels / elapsed.as_secs_f64()
    );
}

fn main() {
    let positive = dabs(256, 1.0);
    let mixed = dabs(256, -1.0);
    measure("positive brush", 512, 512, &positive);
    measure("erase brush", 512, 512, &mixed);
}
