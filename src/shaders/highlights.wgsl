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

    let clipped_count =
        select(0.0, 1.0, r_clipped) +
        select(0.0, 1.0, g_clipped) +
        select(0.0, 1.0, b_clipped);
    let near_clip = smoothstep(vec3<f32>(0.94 * clip), vec3<f32>(clip), sensor_rgb);
    let near_count = near_clip.r + near_clip.g + near_clip.b;
    let unreliable_chroma = smoothstep(1.25, 2.0, max(clipped_count, near_count));

    // Work in the WB-applied camera space. Neutral in this space remains neutral
    // after the camera-to-working matrix, while sensor-space neutral would be
    // tinted by reapplying WB gains.
    let L = (rgb.r + rgb.g + rgb.b) / 3.0;

    var C = SQRT3 * (rgb.r - rgb.g);
    var H = 2.0 * rgb.b - rgb.r - rgb.g;

    let safe_rgb = min(rgb, clip * wb);
    let Cc = SQRT3 * (safe_rgb.r - safe_rgb.g);
    let Hc = 2.0 * safe_rgb.b - safe_rgb.r - safe_rgb.g;

    let denom = C * C + H * H;

    if (denom > 1e-12) {
        let chroma = sqrt(denom);
        let safe_chroma = sqrt(Cc * Cc + Hc * Hc);

        // Never increase chroma.
        let ratio = min(1.0, safe_chroma / chroma);

        C *= ratio * (1.0 - unreliable_chroma);
        H *= ratio * (1.0 - unreliable_chroma);
    }

    // Inverse transform
    let r_out = L - H / 6.0 + C * INV_SQRT12;
    let g_out = L - H / 6.0 - C * INV_SQRT12;
    let b_out = L + H / 3.0;

    let chroma_limited = vec3<f32>(r_out, g_out, b_out);
    let neutral = vec3<f32>(L);

    return max(mix(chroma_limited, neutral, unreliable_chroma), vec3<f32>(0.0));
}
