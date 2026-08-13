//! Shared color math.

fn mul3(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn signed_cuberoot(value: f32) -> f32 {
    value.signum() * value.abs().powf(1.0 / 3.0)
}

fn linear_srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let lms = mul3(
        [
            [0.412_221_46, 0.536_332_55, 0.051_445_99],
            [0.211_903_5, 0.680_699_5, 0.107_396_96],
            [0.088_302_46, 0.281_718_85, 0.629_978_7],
        ],
        rgb,
    )
    .map(signed_cuberoot);
    mul3(
        [
            [0.210_454_26, 0.793_617_8, -0.004_072_05],
            [1.977_998_5, -2.428_592_2, 0.450_593_7],
            [0.025_904_04, 0.782_771_77, -0.808_675_77],
        ],
        lms,
    )
}

fn oklab_to_linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    let root = mul3(
        [
            [1.0, 0.396_337_78, 0.215_803_76],
            [1.0, -0.105_561_35, -0.063_854_17],
            [1.0, -0.089_484_18, -1.291_485_5],
        ],
        lab,
    );
    let lms = root.map(|value| value * value * value);
    mul3(
        [
            [4.076_741_7, -3.307_711_6, 0.230_969_94],
            [-1.268_438, 2.609_757_4, -0.341_319_4],
            [-0.004_196_09, -0.703_418_6, 1.707_614_7],
        ],
        lms,
    )
}

/// Convert scene-linear D65 Rec.2020 RGB to OKLab.
pub fn rec2020_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    linear_srgb_to_oklab(mul3(
        [
            [1.660_491, -0.587_641_1, -0.072_849_9],
            [-0.124_550_5, 1.132_899_9, -0.008_349_4],
            [-0.018_150_8, -0.100_578_9, 1.118_729_7],
        ],
        rgb,
    ))
}

/// Convert OKLab to scene-linear D65 Rec.2020 RGB.
pub fn rec2020_from_oklab(lab: [f32; 3]) -> [f32; 3] {
    mul3(
        [
            [0.627_403_9, 0.329_283, 0.043_313_1],
            [0.069_097_3, 0.919_540_4, 0.011_362_3],
            [0.016_391_4, 0.088_013_3, 0.895_595_3],
        ],
        oklab_to_linear_srgb(lab),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec2020_oklab_round_trips() {
        let sample = [0.2, 0.4, 0.8];
        let reconstructed = rec2020_from_oklab(rec2020_to_oklab(sample));
        for (actual, expected) in reconstructed.into_iter().zip(sample) {
            assert!((actual - expected).abs() <= 2e-5);
        }
    }
}
