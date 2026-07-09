// highlights.wgsl

const SQRT3: f32 = 1.7320508075688772;
const INV_SQRT12: f32 = 0.28867513459481287; // 1 / (2*sqrt(3))

fn sensor_clip_level() -> f32 {
    // UI clip is an offset around normalized sensor white, not the absolute clip level.
    return 0.995 * max(1.0 + params.clip, 0.05);
}

fn reconstruct_sensor_highlights(rgb: vec3<f32>, clip_mask: f32) -> vec3<f32> {
    // Decode clip mask (1 = R, 10 = G, 100 = B)
    let mask = floor(clip_mask + 0.5);
    let r_clipped = mask - 10.0 * floor(mask / 10.0) >= 1.0;
    let g_digit = floor(mask / 10.0) - 10.0 * floor(mask / 100.0);
    let g_clipped = g_digit >= 1.0;
    let b_clipped = floor(mask / 100.0) >= 1.0;

    if (!r_clipped && !g_clipped && !b_clipped) {
        return rgb;
    }

    let wb = params.wb.xyz;
    let sensor_rgb = rgb / max(wb, vec3<f32>(1e-8));
    let clip = sensor_clip_level();

    // Work in the WB-applied camera space. Neutral in this space remains neutral
    // after the camera-to-working matrix, while sensor-space neutral would be
    // tinted by reapplying WB gains.
    let L = (rgb.r + rgb.g + rgb.b) / 3.0;

    var C = SQRT3 * (rgb.r - rgb.g);
    var H = 2.0 * rgb.b - rgb.r - rgb.g;
    let chroma = sqrt(C * C + H * H);
    let saturation = chroma / max(L, 1e-6);
    let low_saturation = 1.0 - smoothstep(0.08, 0.30, saturation);

    let clipped_count =
        select(0.0, 1.0, r_clipped) +
        select(0.0, 1.0, g_clipped) +
        select(0.0, 1.0, b_clipped);
    let near_clip = smoothstep(vec3<f32>(0.975 * clip), vec3<f32>(clip), sensor_rgb);
    let near_count = near_clip.r + near_clip.g + near_clip.b;
    let multi_clipped = smoothstep(1.25, 2.0, clipped_count);
    let all_near_clip = smoothstep(2.35, 3.0, near_count);

    // Clipping alone is not enough evidence to erase chroma: saturated lamps can
    // legitimately clip one or two channels. Neutralize mainly low-saturation
    // multi-channel clips, where the remaining hue is usually demosaic or clip noise.
    let unreliable_chroma = max(multi_clipped, all_near_clip) * low_saturation;

    let safe_rgb = min(rgb, clip * wb);
    let Cc = SQRT3 * (safe_rgb.r - safe_rgb.g);
    let Hc = 2.0 * safe_rgb.b - safe_rgb.r - safe_rgb.g;

    let denom = chroma * chroma;

    if (denom > 1e-12) {
        let safe_chroma = sqrt(Cc * Cc + Hc * Hc);

        // Never increase chroma.
        let ratio = min(1.0, safe_chroma / chroma);

        C *= ratio;
        H *= ratio;
    }

    // Inverse transform
    let r_out = L - H / 6.0 + C * INV_SQRT12;
    let g_out = L - H / 6.0 - C * INV_SQRT12;
    let b_out = L + H / 3.0;

    let chroma_limited = vec3<f32>(r_out, g_out, b_out);
    let neutral = vec3<f32>(L);

    return max(mix(chroma_limited, neutral, unreliable_chroma), vec3<f32>(0.0));
}
