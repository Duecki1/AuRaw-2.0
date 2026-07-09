mod app;
mod pipeline;
mod ui;

use app::AurawApp;

fn main() -> eframe::Result {
    // Initialize env_logger to output debug logs to standard output
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "AuRaw",
        options,
        Box::new(|cc| Ok(Box::new(AurawApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    const REC2020_TO_SRGB: [[f32; 3]; 3] = [
        [1.5489, -0.4830, -0.0657],
        [0.0955, 0.9123, -0.0077],
        [-0.0701, 0.0597, 1.0105],
    ];

    const SRGB_TO_REC2020: [[f32; 3]; 3] = [
        [0.6272773, 0.3292671, 0.0432929],
        [-0.0652639, 1.0613264, 0.0038440],
        [0.0473710, -0.0398610, 0.9923853],
    ];

    #[test]
    fn rec2020_srgb_matrices_are_inverse() {
        for row in 0..3 {
            for col in 0..3 {
                let got = (0..3)
                    .map(|k| REC2020_TO_SRGB[row][k] * SRGB_TO_REC2020[k][col])
                    .sum::<f32>();
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (got - expected).abs() < 1e-5,
                    "matrix product mismatch at {row},{col}: got {got}, expected {expected}"
                );
            }
        }
    }
}
