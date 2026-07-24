use super::{CameraProfile, HsvMap, IccOutputTransform, ProfileEncoding, ToneCurve};
#[cfg(not(target_os = "android"))]
use super::RenderingIntent;

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
fn tone_curve_accepts_partial_domain_and_extends_end_values() {
    let curve = ToneCurve::new(vec![[0.2, 0.1], [0.8, 0.9]]).unwrap();
    let lut = curve.sampled_lut(11);
    assert!((lut[0] - 0.1).abs() < f32::EPSILON);
    assert!((lut[10] - 0.9).abs() < f32::EPSILON);
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
fn default_srgb_output_lut_resolves_near_black_transfer_curve() {
    let lut = IccOutputTransform::srgb();
    for linear in [0.000_1_f32, 0.001, 0.003_130_8, 0.01] {
        let expected = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        let actual = lut.transform_rgb([linear; 3]);
        for channel in actual {
            assert!(
                (channel - expected).abs() < 2e-3,
                "linear {linear}: got {channel}, expected {expected}"
            );
        }
    }
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

#[cfg(not(target_os = "android"))]
#[test]
fn lcms_display_lut_tracks_cpu_reference_with_low_delta_e() {
    use lcms2::{
        CIExyY, CIExyYTRIPLE, Flags, Intent, PixelFormat, Profile, ToneCurve as LcmsToneCurve,
        Transform,
    };

    let output = Profile::new_srgb();
    let bytes = output.icc().expect("serialize LCMS sRGB profile");
    let lut = IccOutputTransform::from_icc(&bytes, RenderingIntent::RelativeColorimetric)
        .expect("build sampled display LUT");

    let white = CIExyY {
        x: 0.3127,
        y: 0.3290,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.708,
            y: 0.292,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.170,
            y: 0.797,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.131,
            y: 0.046,
            Y: 1.0,
        },
    };
    let linear = LcmsToneCurve::new(1.0);
    let input = Profile::new_rgb(&white, &primaries, &[&linear, &linear, &linear])
        .expect("create linear Rec.2020 profile");
    let reference: Transform<[f32; 3], [f32; 3]> = Transform::new_flags(
        &input,
        PixelFormat::RGB_FLT,
        &output,
        PixelFormat::RGB_FLT,
        Intent::RelativeColorimetric,
        Flags::HIGHRES_PRECALC | Flags::BLACKPOINT_COMPENSATION,
    )
    .expect("build LCMS CPU reference transform");

    let samples = [
        [0.01, 0.01, 0.01],
        [0.18, 0.18, 0.18],
        [0.5, 0.5, 0.5],
        [0.22, 0.31, 0.12],
        [0.42, 0.21, 0.09],
        [0.08, 0.19, 0.35],
    ];
    let mut expected = [[0.0_f32; 3]; 6];
    reference.transform_pixels(&samples, &mut expected);

    for (input_rgb, expected_rgb) in samples.into_iter().zip(expected) {
        let sampled_rgb = lut.transform_rgb(input_rgb);
        let delta_e = delta_e76_srgb(sampled_rgb, expected_rgb);
        assert!(
            delta_e < 1.0,
            "sample {input_rgb:?}: sampled={sampled_rgb:?} reference={expected_rgb:?} ΔE76={delta_e}"
        );
    }
}

#[cfg(not(target_os = "android"))]
fn delta_e76_srgb(a: [f32; 3], b: [f32; 3]) -> f32 {
    let lab = |rgb: [f32; 3]| {
        let linear = rgb.map(|v| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        });
        let xyz = [
            0.4124564 * linear[0] + 0.3575761 * linear[1] + 0.1804375 * linear[2],
            0.2126729 * linear[0] + 0.7151522 * linear[1] + 0.0721750 * linear[2],
            0.0193339 * linear[0] + 0.1191920 * linear[1] + 0.9503041 * linear[2],
        ];
        let f = |value: f32| {
            if value > 216.0 / 24389.0 {
                value.cbrt()
            } else {
                (24389.0 / 27.0 * value + 16.0) / 116.0
            }
        };
        let fx = f(xyz[0] / 0.95047);
        let fy = f(xyz[1]);
        let fz = f(xyz[2] / 1.08883);
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    };
    let a = lab(a);
    let b = lab(b);
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
