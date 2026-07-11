// Shared data for image-adaptive tonal controls. The histogram is calculated
// from unexposed scene-linear luminance, so the Exposure slider remains a true
// exposure control instead of being normalized away by the analysis pass.
const SCENE_MIDDLE_GREY: f32 = 0.1842;
const DISPLAY_MIDDLE_GREY: f32 = 0.1842;

const TONE_HISTOGRAM_BIN_COUNT: u32 = 256u;
const TONE_EV_MIN: f32 = -16.0;
const TONE_EV_MAX: f32 = 12.0;
const TONE_EV_RANGE: f32 = TONE_EV_MAX - TONE_EV_MIN;

struct ToneStats {
    // 0.5%, 5%, 50%, 95% scene-luminance percentiles in EV.
    percentiles_0: vec4<f32>,
    // 99.5%, robust dynamic range, sampled-pixel count, reserved.
    percentiles_1: vec4<f32>,
}

struct TonePercentiles {
    p005: f32,
    p05: f32,
    p50: f32,
    p95: f32,
    p995: f32,
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
