
fn edge_glow_log_luminance_at(pos: vec2<i32>) -> f32 {
    return log2(max(Common::safe_luma(SceneAdjustments::local_effects_at(pos)), 1e-6));
}

fn edge_glow_sobel_energy(pos: vec2<i32>, radius: i32) -> f32 {
    let x = vec2<i32>(radius, 0);
    let y = vec2<i32>(0, radius);
    let tl = edge_glow_log_luminance_at(pos - x - y);
    let tc = edge_glow_log_luminance_at(pos - y);
    let tr = edge_glow_log_luminance_at(pos + x - y);
    let ml = edge_glow_log_luminance_at(pos - x);
    let mr = edge_glow_log_luminance_at(pos + x);
    let bl = edge_glow_log_luminance_at(pos - x + y);
    let bc = edge_glow_log_luminance_at(pos + y);
    let br = edge_glow_log_luminance_at(pos + x + y);
    let gradient = vec2<f32>(
        -tl - 2.0 * ml - bl + tr + 2.0 * mr + br,
        -tl - 2.0 * tc - tr + bl + 2.0 * bc + br,
    );
    return length(gradient) * 0.125;
}

fn apply_edge_glow(
    pos: vec2<i32>,
    source_rgb: vec3<f32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
) -> vec3<f32> {
    let amount = clamp(primary.x / 100.0, 0.0, 1.0);
    if amount <= 1e-6 {
        return source_rgb;
    }

    let edge_width = clamp(primary.y, 0.5, 8.0);
    let detail = clamp(primary.z / 100.0, 0.0, 1.0);
    let glow = clamp(primary.w / 100.0, 0.0, 1.0);
    let inner_radius = SceneAdjustments::presence_step(edge_width, 24);
    let outer_radius = min(inner_radius * 2, 48);
    let threshold = mix(0.22, 0.018, detail);
    let inner_energy = edge_glow_sobel_energy(pos, inner_radius);
    let outer_energy = edge_glow_sobel_energy(pos, outer_radius);
    let core = smoothstep(threshold, threshold * 2.75 + 0.02, inner_energy);
    let halo = smoothstep(threshold * 0.42, threshold * 1.45 + 0.01, outer_energy);
    let emission = max(core, halo * glow * 0.78);
    let color = mask_effect_picker_color_to_working(secondary.xyz);
    let emitted = color * emission * amount * 1.8;
    return Color::perceptual_gamut_compress_nonnegative_rec2020(source_rgb + emitted);
}
