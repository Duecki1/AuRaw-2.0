#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result {
    auraw::run_desktop()
}

#[cfg(target_os = "android")]
fn main() {}

#[cfg(test)]
mod tests {
    const REC2020_TO_SRGB: [[f32; 3]; 3] = [
        [1.6604910, -0.5876411, -0.0728499],
        [-0.1245505, 1.1328999, -0.0083494],
        [-0.0181508, -0.1005789, 1.1187297],
    ];

    const SRGB_TO_REC2020: [[f32; 3]; 3] = [
        [0.6274039, 0.3292830, 0.0433131],
        [0.0690973, 0.9195404, 0.0113623],
        [0.0163914, 0.0880133, 0.8955953],
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

    #[test]
    fn srgb_primaries_map_to_standard_rec2020_coordinates() {
        let expected = [
            [0.6274039, 0.0690973, 0.0163914],
            [0.3292830, 0.9195404, 0.0880133],
            [0.0433131, 0.0113623, 0.8955953],
        ];

        for primary in 0..3 {
            for channel in 0..3 {
                let got = SRGB_TO_REC2020[channel][primary];
                assert!(
                    (got - expected[primary][channel]).abs() < 1e-7,
                    "sRGB primary {primary} mapped to {got} in Rec.2020 channel {channel}"
                );
            }
        }
    }
}
