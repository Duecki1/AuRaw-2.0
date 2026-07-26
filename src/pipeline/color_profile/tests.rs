#[cfg(not(target_os = "android"))]
use super::RenderingIntent;
use super::{
    map_output_lut_input_rec2020, output_lut_shaper, CameraProfile, HsvMap, IccOutputTransform,
    ProfileEncoding, ToneCurve,
};

fn identity_encoded_lut(edge: u32) -> IccOutputTransform {
    let mut entries = Vec::with_capacity((edge * edge * edge) as usize);
    let denominator = (edge - 1) as f32;
    for b in 0..edge {
        for g in 0..edge {
            for r in 0..edge {
                entries.push([
                    r as f32 / denominator,
                    g as f32 / denominator,
                    b as f32 / denominator,
                    0.0,
                ]);
            }
        }
    }
    IccOutputTransform {
        size: edge,
        entries,
    }
}

fn constant_encoded_lut(edge: u32, value: [f32; 3]) -> IccOutputTransform {
    IccOutputTransform {
        size: edge,
        entries: vec![[value[0], value[1], value[2], 0.0]; (edge * edge * edge) as usize],
    }
}

fn shader_reference_sample(entries: &[[f32; 4]], edge: u32, rgb: [f32; 3]) -> [f32; 3] {
    let mapped = map_output_lut_input_rec2020(rgb);
    let shaped = mapped.map(output_lut_shaper);
    let coordinate = shaped.map(|value| value.clamp(0.0, 1.0) * (edge - 1) as f32);
    let low = coordinate.map(|value| value.floor() as u32);
    let high = low.map(|value| (value + 1).min(edge - 1));
    let fraction = [
        coordinate[0] - low[0] as f32,
        coordinate[1] - low[1] as f32,
        coordinate[2] - low[2] as f32,
    ];
    let fetch = |r: u32, g: u32, b: u32| {
        let entry = entries[((b * edge + g) * edge + r) as usize];
        [entry[0], entry[1], entry[2]]
    };
    let mix = |left: [f32; 3], right: [f32; 3], amount: f32| {
        [
            left[0] * (1.0 - amount) + right[0] * amount,
            left[1] * (1.0 - amount) + right[1] * amount,
            left[2] * (1.0 - amount) + right[2] * amount,
        ]
    };
    let low_z = mix(
        mix(
            fetch(low[0], low[1], low[2]),
            fetch(high[0], low[1], low[2]),
            fraction[0],
        ),
        mix(
            fetch(low[0], high[1], low[2]),
            fetch(high[0], high[1], low[2]),
            fraction[0],
        ),
        fraction[1],
    );
    let high_z = mix(
        mix(
            fetch(low[0], low[1], high[2]),
            fetch(high[0], low[1], high[2]),
            fraction[0],
        ),
        mix(
            fetch(low[0], high[1], high[2]),
            fetch(high[0], high[1], high[2]),
            fraction[0],
        ),
        fraction[1],
    );
    mix(low_z, high_z, fraction[2])
}

fn assert_rgb_close(actual: [f32; 3], expected: [f32; 3], tolerance: f32) {
    for channel in 0..3 {
        assert!(
            (actual[channel] - expected[channel]).abs() <= tolerance,
            "channel {channel}: actual={actual:?}, expected={expected:?}, tolerance={tolerance}"
        );
    }
}

#[test]
fn identity_like_output_lut_preserves_primaries_black_white_and_neutral_axis() {
    let lut = identity_encoded_lut(5);
    for primary in [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
    ] {
        assert_rgb_close(lut.transform_rgb(primary), primary, 1e-6);
    }

    let gray = lut.transform_rgb([0.18, 0.18, 0.18]);
    assert!((gray[0] - gray[1]).abs() <= 1e-7);
    assert!((gray[1] - gray[2]).abs() <= 1e-7);
}

#[test]
fn encoded_destination_primaries_are_returned_without_post_lut_linear_srgb_processing() {
    for encoded_primary in [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.5, 0.5, 0.5],
    ] {
        let lut = constant_encoded_lut(3, encoded_primary);
        assert_rgb_close(lut.transform_rgb([0.37, 0.42, 0.19]), encoded_primary, 1e-7);
    }
}

#[test]
fn cpu_and_gpu_reference_lut_sampling_agree_at_every_cube_corner() {
    let lut = identity_encoded_lut(7);
    for blue in [0.0, 1.0] {
        for green in [0.0, 1.0] {
            for red in [0.0, 1.0] {
                let input = [red, green, blue];
                assert_rgb_close(
                    lut.transform_rgb(input),
                    shader_reference_sample(&lut.entries, lut.size, input),
                    1e-7,
                );
            }
        }
    }
}

#[test]
fn cpu_and_gpu_reference_lut_sampling_agree_for_deterministic_random_points() {
    let edge = 9;
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as u32) as f32 / u32::MAX as f32
    };
    let mut entries = Vec::with_capacity((edge * edge * edge) as usize);
    for _ in 0..edge * edge * edge {
        entries.push([next(), next(), next(), 0.0]);
    }
    let lut = IccOutputTransform {
        size: edge,
        entries,
    };
    for _ in 0..512 {
        let input = [next(), next(), next()];
        assert_rgb_close(
            lut.transform_rgb(input),
            shader_reference_sample(&lut.entries, lut.size, input),
            2e-6,
        );
    }
}

#[test]
fn cpu_and_gpu_reference_lut_sampling_agree_for_out_of_gamut_points() {
    let edge = 7;
    let mut state = 0xbb67_ae85_84ca_a73b_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as u32) as f32 / u32::MAX as f32
    };
    let mut entries = Vec::with_capacity((edge * edge * edge) as usize);
    for _ in 0..edge * edge * edge {
        entries.push([next(), next(), next(), 0.0]);
    }
    let lut = IccOutputTransform {
        size: edge,
        entries,
    };
    for _ in 0..256 {
        let input = [next() * 2.0 - 0.5, next() * 2.0 - 0.5, next() * 2.0 - 0.5];
        assert_rgb_close(
            lut.transform_rgb(input),
            shader_reference_sample(&lut.entries, lut.size, input),
            3e-6,
        );
    }
}

#[test]
fn exact_unit_cube_edges_select_valid_terminal_cells() {
    let edge = 4;
    let mut entries = Vec::with_capacity((edge * edge * edge) as usize);
    for b in 0..edge {
        for g in 0..edge {
            for r in 0..edge {
                entries.push([r as f32, g as f32, b as f32, 0.0]);
            }
        }
    }
    let lut = IccOutputTransform {
        size: edge,
        entries,
    };
    assert_rgb_close(lut.transform_rgb([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0], 0.0);
    assert_rgb_close(lut.transform_rgb([1.0, 1.0, 1.0]), [3.0, 3.0, 3.0], 0.0);
    assert_rgb_close(lut.transform_rgb([1.0, 0.0, 1.0]), [3.0, 0.0, 3.0], 0.0);
}

#[test]
fn pre_lut_gamut_policy_preserves_in_cube_values_and_maps_outliers_to_unit_rec2020() {
    let in_cube = [0.0, 1.0, 0.25];
    assert_rgb_close(map_output_lut_input_rec2020(in_cube), in_cube, 0.0);
    for input in [[1.4, -0.2, 0.3], [-0.5, 0.8, 1.7], [2.0, 2.0, 2.0]] {
        let mapped = map_output_lut_input_rec2020(input);
        assert!(mapped.iter().all(|value| value.is_finite()));
        assert!(mapped
            .iter()
            .all(|value| (-1e-5..=1.000_01).contains(value)));
    }
}

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
