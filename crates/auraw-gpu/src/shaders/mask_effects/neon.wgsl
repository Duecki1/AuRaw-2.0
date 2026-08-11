// A scene-linear, scale-aware Neon effect. It derives edge energy from the
// current developed image and returns a new working-space value. This file is
// appended to creative_effects.wgsl and deliberately reuses its imported
// Common, Color, and SceneAdjustments modules. Source pixels are never modified
// or baked into the edit.

fn encoded_srgb_component_to_linear(value: f32) -> f32 {
    let encoded = clamp(value, 0.0, 1.0);
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

fn picker_color_to_working(color: vec3<f32>) -> vec3<f32> {
    let linear_srgb = vec3<f32>(
        encoded_srgb_component_to_linear(color.r),
        encoded_srgb_component_to_linear(color.g),
        encoded_srgb_component_to_linear(color.b),
    );
    return max(Common::SRGB_TO_REC2020 * linear_srgb, vec3<f32>(0.0));
}

fn log_luminance_at(pos: vec2<i32>) -> f32 {
    return log2(max(Common::safe_luma(SceneAdjustments::adjustment_base_at(pos)), 1e-6));
}

fn sobel_edge_energy(pos: vec2<i32>, radius: i32) -> f32 {
    let x = vec2<i32>(radius, 0);
    let y = vec2<i32>(0, radius);
    let tl = log_luminance_at(pos - x - y);
    let tc = log_luminance_at(pos - y);
    let tr = log_luminance_at(pos + x - y);
    let ml = log_luminance_at(pos - x);
    let mr = log_luminance_at(pos + x);
    let bl = log_luminance_at(pos - x + y);
    let bc = log_luminance_at(pos + y);
    let br = log_luminance_at(pos + x + y);
    let gradient = vec2<f32>(
        -tl - 2.0 * ml - bl + tr + 2.0 * mr + br,
        -tl - 2.0 * tc - tr + bl + 2.0 * bc + br,
    );
    return length(gradient) * 0.125;
}

fn apply_neon(
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
    let background = clamp(secondary.w / 100.0, 0.0, 1.0);
    let inner_radius = SceneAdjustments::presence_step(edge_width, 24);
    let outer_radius = min(inner_radius * 2, 48);

    // Detail lowers the detection threshold; the broader second sample gives
    // Glow a halo without introducing a separate destructive blur layer.
    let threshold = mix(0.22, 0.018, detail);
    let inner_energy = sobel_edge_energy(pos, inner_radius);
    let outer_energy = sobel_edge_energy(pos, outer_radius);
    let core = smoothstep(threshold, threshold * 2.75 + 0.02, inner_energy);
    let halo = smoothstep(threshold * 0.45, threshold * 1.55 + 0.012, outer_energy);
    let emission = max(core, halo * glow * 0.72);

    let neon_color = picker_color_to_working(secondary.xyz);
    let source_luma = max(Common::safe_luma(source_rgb), 0.0);
    let edge_visibility = mix(0.78, 1.12, clamp(sqrt(source_luma), 0.0, 1.0));
    let emitted = neon_color * emission * edge_visibility * (0.45 + 2.35 * amount);
    let retained = source_rgb * background;
    return Color::perceptual_gamut_compress_nonnegative_rec2020(retained + emitted);
}
