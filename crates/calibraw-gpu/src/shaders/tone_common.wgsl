const SCENE_MIDDLE_GREY: f32 = 0.1845;
const DISPLAY_MIDDLE_GREY: f32 = 0.1845;

const TONE_HISTOGRAM_BIN_COUNT: u32 = 256u;
const TONE_EV_MIN: f32 = -16.0;
const TONE_EV_MAX: f32 = 12.0;
const TONE_EV_RANGE: f32 = TONE_EV_MAX - TONE_EV_MIN;

// Naga rewrites exported identifiers ending in digits.
struct ToneStats {
    percentiles_0_field: vec4<f32>,
    percentiles_1_field: vec4<f32>,
}

struct TonePercentiles {
    p005_field: f32,
    p05_field: f32,
    p50_field: f32,
    p95_field: f32,
    p995_field: f32,
}

fn tone_ev_to_bin(ev: f32) -> u32 {
    let normalized = clamp((ev - TONE_EV_MIN) / TONE_EV_RANGE, 0.0, 0.999999);
    return min(u32(normalized * f32(TONE_HISTOGRAM_BIN_COUNT)), TONE_HISTOGRAM_BIN_COUNT - 1u);
}

fn tone_bin_to_ev(bin: u32) -> f32 {
    return TONE_EV_MIN
        + (f32(bin) + 0.5) * TONE_EV_RANGE / f32(TONE_HISTOGRAM_BIN_COUNT);
}

fn tone_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let width = max(edge1 - edge0, 1e-4);
    let x = clamp((value - edge0) / width, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}
