
use super::{CameraProfile, HsvMap, IccOutputTransform, ProfileEncoding, ToneCurve};

#[test]
fn dual_illuminant_maps_interpolate_entrywise() {
    let a = HsvMap::new(
        [1, 2, 1],
        vec![[0.0, 1.0, 1.0], [10.0, 0.5, 1.0]],
        ProfileEncoding::Linear,
    )
    .unwrap();
    let b = HsvMap::new(
        [1, 2, 1],
        vec![[20.0, 2.0, 1.0], [30.0, 1.5, 2.0]],
        ProfileEncoding::Linear,
    )
    .unwrap();
    let mixed = HsvMap::interpolate(&a, &b, 0.25).unwrap();
    assert_eq!(mixed.entries[0], [5.0, 1.25, 1.0]);
}

#[test]
fn tone_curve_sampling_is_monotonic_for_monotonic_points() {
    let curve = ToneCurve::new(vec![[0.0, 0.0], [0.2, 0.08], [0.7, 0.82], [1.0, 1.0]]).unwrap();
    let lut = curve.sampled_lut(256);
    assert!(lut.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6));
}

#[test]
fn default_srgb_output_lut_preserves_neutral_order() {
    let lut = IccOutputTransform::srgb();
    let low = lut.transform_rgb([0.1; 3]);
    let high = lut.transform_rgb([0.5; 3]);
    assert!(low[0] < high[0]);
    assert!((low[0] - low[1]).abs() < 0.02);
}

#[test]
fn gpu_layout_offsets_are_contiguous() {
    let profile = CameraProfile {
        hue_sat_maps: [
            Some(
                HsvMap::new([1, 2, 1], vec![[0.0, 1.0, 1.0]; 2], ProfileEncoding::Linear).unwrap(),
            ),
            Some(
                HsvMap::new([1, 2, 1], vec![[0.0, 1.0, 1.0]; 2], ProfileEncoding::Linear).unwrap(),
            ),
        ],
        tone_curve: Some(ToneCurve::new(vec![[0.0, 0.0], [1.0, 1.0]]).unwrap()),
        ..Default::default()
    };
    let data = profile.gpu_data(&IccOutputTransform::srgb());
    assert_eq!(data.layout.hue_sat[3], 1);
    assert_eq!(data.layout.hue_sat_2[3], 3);
    assert_eq!(data.words[0].map(f32::to_bits), data.layout.hue_sat_2);
    assert_eq!(data.layout.tone[1], 5);
    assert_eq!(
        data.words.len(),
        data.layout.output[3] as usize + 33usize.pow(3)
    );
}
