#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result {
    let args = std::env::args().collect::<Vec<_>>();
    if let Some(code) = auraw::run_onnx_runtime_probe_cli(&args) {
        std::process::exit(code);
    }
    auraw::run_desktop()
}

#[cfg(target_os = "android")]
fn main() {}

#[cfg(test)]
mod tests {
    const REC2020_TO_SRGB: [[f32; 3]; 3] = [
        [1.660_491, -0.5876411, -0.0728499],
        [-0.1245505, 1.1328999, -0.0083494],
        [-0.0181508, -0.1005789, 1.1187297],
    ];

    const SRGB_TO_REC2020: [[f32; 3]; 3] = [
        [0.6274039, 0.329_283, 0.0433131],
        [0.0690973, 0.9195404, 0.0113623],
        [0.0163914, 0.0880133, 0.8955953],
    ];

    #[test]
    fn rec2020_srgb_matrices_are_inverse() {
        for (row, transform_row) in REC2020_TO_SRGB.iter().enumerate() {
            for (col, _) in SRGB_TO_REC2020[0].iter().enumerate() {
                let got = transform_row
                    .iter()
                    .enumerate()
                    .map(|(k, value)| value * SRGB_TO_REC2020[k][col])
                    .sum::<f32>();
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (got - expected).abs() < 1e-5,
                    "matrix product mismatch at {row},{col}: got {got}, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn srgb_primaries_map_to_standard_rec2020_coordinates() {
        let expected = [
            [0.6274039, 0.0690973, 0.0163914],
            [0.329_283, 0.9195404, 0.0880133],
            [0.0433131, 0.0113623, 0.8955953],
        ];

        for (primary, expected_primary) in expected.iter().enumerate() {
            for (channel, transform_row) in SRGB_TO_REC2020.iter().enumerate() {
                let got = transform_row[primary];
                assert!(
                    (got - expected_primary[channel]).abs() < 1e-7,
                    "sRGB primary {primary} mapped to {got} in Rec.2020 channel {channel}"
                );
            }
        }
    }
}
