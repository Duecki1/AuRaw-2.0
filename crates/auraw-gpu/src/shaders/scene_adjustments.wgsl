#import auraw::common as Common
#import auraw::color as Color
#import auraw::profile as Profile
#import auraw::basic_adjustments as BasicAdjustments
#import auraw::tonemap as Tonemap
#import auraw::detail_capture as DetailCapture



@group(0) @binding(11) var scene_tex: texture_2d<f32>;
@group(0) @binding(12) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(21) var adjustment_base_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(23) var local_effects_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(24) var local_effects_tex: texture_2d<f32>;
@group(0) @binding(25) var creative_effects_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(26) var final_adjustment_tex: texture_2d<f32>;
@group(0) @binding(27) var local_mask_tex: texture_2d_array<f32>;
@group(0) @binding(28) var local_mask_sampler: sampler;
@group(0) @binding(29) var display_linear_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(30) var glow_work_tex: texture_2d<f32>;
@group(0) @binding(31) var glow_work_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(34) var light_rays_mask_tex: texture_2d_array<f32>;

fn local_mask_uv(pos: vec2<i32>) -> vec2<f32> {
    let full_size = vec2<f32>(
        f32(max(Common::camera_uniforms.full_width, 1u)),
        f32(max(Common::camera_uniforms.full_height, 1u)),
    );
    let global_pos = vec2<f32>(pos + Common::tile_origin()) + vec2<f32>(0.5);
    let full_uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    if Common::scene_tone_uniforms.mask_counts.w == 0u {
        return full_uv;
    }
    let packed_min = Common::scene_tone_uniforms.mask_counts.y;
    let packed_max = Common::scene_tone_uniforms.mask_counts.z;
    let rect_min = vec2<f32>(
        f32(packed_min & 65535u),
        f32(packed_min >> 16u),
    ) / 65535.0;
    let rect_max = vec2<f32>(
        f32(packed_max & 65535u),
        f32(packed_max >> 16u),
    ) / 65535.0;
    return (full_uv - rect_min) / max(rect_max - rect_min, vec2<f32>(1.0 / 65535.0));
}

fn local_mask_texture_uv(region_uv: vec2<f32>) -> vec2<f32> {
    if Common::scene_tone_uniforms.mask_counts.w == 0u {
        return region_uv;
    }
    let atlas_size_u = textureDimensions(local_mask_tex);
    var valid_size_u = atlas_size_u;
    if Common::scene_tone_uniforms.mask_counts.w != 0xffffffffu {
        valid_size_u = vec2<u32>(
            Common::scene_tone_uniforms.mask_counts.w & 65535u,
            Common::scene_tone_uniforms.mask_counts.w >> 16u,
        );
    }
    let atlas_size = vec2<f32>(max(atlas_size_u, vec2<u32>(1u)));
    let valid_size = vec2<f32>(max(valid_size_u, vec2<u32>(1u)));
    let half_texel = vec2<f32>(0.5) / valid_size;
    let safe_region_uv = clamp(region_uv, half_texel, vec2<f32>(1.0) - half_texel);
    return safe_region_uv * valid_size / atlas_size;
}

fn local_mask_weight(pos: vec2<i32>, index: u32) -> f32 {
    let uv = local_mask_uv(pos);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 0.0;
    }
    return textureSampleLevel(
        local_mask_tex,
        local_mask_sampler,
        local_mask_texture_uv(uv),
        i32(index),
        0.0,
    ).x;
}

fn apply_local_exposure_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != 0u { continue; }
        let value = clamp(Common::mask_data[index].adjust_0_field.x, -5.0, 5.0);
        if abs(value) <= 1e-7 { continue; }
        let weight = local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        let adjusted = rgb * exp2(value);
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

fn raster_ca_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let local_extent = vec2<f32>(
        f32(Common::camera_uniforms.width - 1u),
        f32(Common::camera_uniforms.height - 1u),
    );
    let origin = vec2<f32>(
        f32(Common::camera_uniforms.tile_origin_x),
        f32(Common::camera_uniforms.tile_origin_y),
    );
    let full_extent = vec2<f32>(
        f32(Common::camera_uniforms.full_width - 1u),
        f32(Common::camera_uniforms.full_height - 1u),
    );
    let center = 0.5 * full_extent;
    let global_pos = vec2<f32>(pos) + origin;
    let rel = global_pos - center;
    let norm = rel / max(center, vec2<f32>(1.0));
    let scale = 1.0 + amount * 0.001 * dot(norm, norm);
    let warped_global = clamp(center + rel * scale, vec2<f32>(0.0), full_extent);
    return clamp(warped_global - origin, vec2<f32>(0.0), local_extent);
}

fn raster_scene_bilinear(pos: vec2<f32>) -> vec3<f32> {
    let base = floor(pos);
    let p0 = vec2<i32>(i32(base.x), i32(base.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let f = fract(pos);
    let a = textureLoad(scene_tex, Common::clamp_pos(p0), 0).xyz;
    let b = textureLoad(scene_tex, Common::clamp_pos(vec2<i32>(p1.x, p0.y)), 0).xyz;
    let c = textureLoad(scene_tex, Common::clamp_pos(vec2<i32>(p0.x, p1.y)), 0).xyz;
    let d = textureLoad(scene_tex, Common::clamp_pos(p1), 0).xyz;
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn source_scene_at(pos: vec2<i32>) -> vec3<f32> {
    var rgb = textureLoad(scene_tex, Common::clamp_pos(pos), 0).xyz;
    if Common::camera_uniforms._pad_0_field <= 0.5 {
        return rgb;
    }
    if abs(Common::camera_uniforms.ca_red) > 1e-6 {
        rgb.r = raster_scene_bilinear(
            raster_ca_warped_pos(pos, Common::camera_uniforms.ca_red),
        ).r;
    }
    if abs(Common::camera_uniforms.ca_blue) > 1e-6 {
        rgb.b = raster_scene_bilinear(
            raster_ca_warped_pos(pos, Common::camera_uniforms.ca_blue),
        ).b;
    }
    return rgb;
}

fn scene_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = source_scene_at(pos);
    var working = Color::cam_to_working(camera_rgb);


    if Common::camera_uniforms._pad_0_field > 0.5 {
        working = BasicAdjustments::apply_temperature_tint_values(
            working,
            Common::camera_uniforms.temperature,
            Common::camera_uniforms.tint,
        );
    }
    return working;
}

fn adjustment_base_at(pos: vec2<i32>) -> vec3<f32> {
    // Binding 22 is stage-relative: pre-tone here, post-tone in the presence pass.
    return DetailCapture::adjustment_base_at(pos);
}

fn local_effects_at(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(local_effects_tex, Common::clamp_pos(pos), 0).xyz;
}

fn log_luminance(rgb: vec3<f32>) -> f32 {
    return log2(Common::safe_luma(rgb));
}

fn presence_reference_scale() -> f32 {
    return clamp(
        f32(min(Common::camera_uniforms.full_width, Common::camera_uniforms.full_height)) / 1080.0,
        0.55,
        3.0,
    );
}

fn presence_step(reference_pixels: f32, maximum: i32) -> i32 {
    return clamp(
        i32(round(reference_pixels * presence_reference_scale())),
        1,
        maximum,
    );
}

fn bilateral_log_luminance(
    pos: vec2<i32>,
    radius: i32,
    step: i32,
    range_strength: f32,
) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    let sigma = max(f32(radius) * 0.72, 0.85);
    var sum = 0.0;
    var sum_w = 0.0;

    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            if abs(dx) > radius || abs(dy) > radius { continue; }
            let sample_ev = log_luminance(
                adjustment_base_at(pos + vec2<i32>(dx * step, dy * step)),
            );
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = exp(-0.5 * distance_squared / (sigma * sigma));
            let delta = sample_ev - center;
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn atrous_kernel_weight(offset: i32) -> f32 {
    switch abs(offset) {
        case 0: { return 6.0; }
        case 1: { return 4.0; }
        default: { return 1.0; }
    }
}

fn atrous_log_luminance(pos: vec2<i32>, step: i32, range_strength: f32) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    var sum = 0.0;
    var sum_w = 0.0;
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            let sample_ev = log_luminance(adjustment_base_at(sample_pos));
            let delta = sample_ev - center;
            let spatial = atrous_kernel_weight(kx) * atrous_kernel_weight(ky);
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn soft_detail_threshold(detail: f32, threshold: f32) -> f32 {
    return sign(detail) * max(abs(detail) - threshold, 0.0);
}

fn local_curve_block(mask_index: u32, curve: u32, block: u32) -> vec4<f32> {
    if curve == 1u { return Common::mask_data[mask_index].curves_red[block]; }
    if curve == 2u { return Common::mask_data[mask_index].curves_green[block]; }
    if curve == 3u { return Common::mask_data[mask_index].curves_blue[block]; }
    return Common::mask_data[mask_index].curves[block];
}

fn local_curve_point(mask_index: u32, curve: u32, index: u32) -> vec2<f32> {
    let packed = local_curve_block(mask_index, curve, index / 2u);
    return select(packed.xy, packed.zw, (index & 1u) != 0u);
}

fn local_curve_secant(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return (b.y - a.y) / max(b.x - a.x, 1e-5);
}

fn local_curve_tangent(mask_index: u32, curve: u32, index: u32, count: u32) -> f32 {
    if index == 0u {
        let endpoint = local_curve_point(mask_index, curve, 0u);
        let raw_slope = local_curve_secant(
            endpoint,
            local_curve_point(mask_index, curve, 1u),
        );
        return Tonemap::limit_scene_curve_endpoint_tangent(endpoint.y, raw_slope);
    }
    if index + 1u >= count {
        return local_curve_secant(
            local_curve_point(mask_index, curve, count - 2u),
            local_curve_point(mask_index, curve, count - 1u),
        );
    }
    let previous = local_curve_secant(
        local_curve_point(mask_index, curve, index - 1u),
        local_curve_point(mask_index, curve, index),
    );
    let next = local_curve_secant(
        local_curve_point(mask_index, curve, index),
        local_curve_point(mask_index, curve, index + 1u),
    );
    if previous * next <= 0.0 {
        return 0.0;
    }
    return 2.0 * previous * next / max(abs(previous + next), 1e-6) * sign(previous + next);
}

fn local_curve_value(mask_index: u32, curve: u32, input: f32) -> f32 {
    let count = u32(clamp(local_curve_block(mask_index, curve, 4u).x, 2.0, 8.0));
    let x = clamp(input, 0.0, 1.0);
    var segment = count - 2u;
    for (var index = 0u; index + 1u < count; index = index + 1u) {
        if x <= local_curve_point(mask_index, curve, index + 1u).x {
            segment = index;
            break;
        }
    }

    let p0 = local_curve_point(mask_index, curve, segment);
    let p1 = local_curve_point(mask_index, curve, segment + 1u);
    let width = max(p1.x - p0.x, 1e-5);
    let t = clamp((x - p0.x) / width, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let m0 = local_curve_tangent(mask_index, curve, segment, count) * width;
    let m1 = local_curve_tangent(mask_index, curve, segment + 1u, count) * width;
    let hermite = (2.0 * t3 - 3.0 * t2 + 1.0) * p0.y
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * p1.y
        + (t3 - t2) * m1;
    return clamp(hermite, min(p0.y, p1.y), max(p0.y, p1.y));
}

fn local_scene_curve_zero_slope(mask_index: u32, curve: u32) -> f32 {
    let count = u32(clamp(local_curve_block(mask_index, curve, 4u).x, 2.0, 8.0));
    let encoded_black = local_curve_value(mask_index, curve, 0.0);
    let encoded_slope = local_curve_tangent(mask_index, curve, 0u, count);
    return Tonemap::decoded_scene_curve_zero_slope(encoded_black, encoded_slope);
}

fn apply_local_scene_channel_curve(mask_index: u32, curve: u32, value: f32) -> f32 {
    let encoded_black = local_curve_value(mask_index, curve, 0.0);
    let black = Tonemap::scene_curve_decode(encoded_black);
    if value < 0.0 {
        return Tonemap::clamp_scene_curve_value(
            black + value * local_scene_curve_zero_slope(mask_index, curve),
        );
    }
    return Tonemap::scene_curve_decode(
        local_curve_value(mask_index, curve, Tonemap::scene_curve_encode(value)),
    );
}

fn apply_local_curves_for_mask(mask_index: u32, input_rgb: vec3<f32>) -> vec3<f32> {
    var adjusted = input_rgb;
    let state = Common::mask_data[mask_index].metadata;
    if (state.z & 1u) != 0u {
        let luminance = max(dot(adjusted, Common::LUMA), 0.0);
        let encoded_black = local_curve_value(mask_index, 0u, 0.0);
        let black_luminance = Tonemap::scene_curve_decode(encoded_black);
        let curved = Tonemap::scene_curve_decode(
            local_curve_value(mask_index, 0u, Tonemap::scene_curve_encode(luminance)),
        );
        adjusted = Tonemap::remap_scene_luminance(
            adjusted,
            curved,
            black_luminance,
            max(local_scene_curve_zero_slope(mask_index, 0u), 0.0),
        );
    }
    if (state.z & 2u) != 0u {
        adjusted.r = apply_local_scene_channel_curve(mask_index, 1u, adjusted.r);
    }
    if (state.z & 4u) != 0u {
        adjusted.g = apply_local_scene_channel_curve(mask_index, 2u, adjusted.g);
    }
    if (state.z & 8u) != 0u {
        adjusted.b = apply_local_scene_channel_curve(mask_index, 3u, adjusted.b);
    }
    return adjusted;
}

fn apply_local_scene_tone_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != 0u { continue; }
        let weight = local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }

        rgb = Tonemap::apply_local_basic_tone_values_with_low_strength(
            rgb,
            pos,
            0.0,
            Common::mask_data[index].adjust_0_field.w,
            0.0,
            0.0,
            weight,
        );

        var adjusted = Tonemap::apply_local_basic_tone_values(
            rgb,
            pos,
            Common::mask_data[index].adjust_0_field.z,
            0.0,
            Common::mask_data[index].adjust_1_field.x,
            0.0,
        );
        adjusted = Tonemap::apply_mask_contrast_value(adjusted, Common::mask_data[index].adjust_0_field.y);
        adjusted = BasicAdjustments::apply_temperature_tint_values(
            adjusted,
            Common::mask_data[index].adjust_1_field.z,
            Common::mask_data[index].adjust_1_field.w,
        );
        adjusted = apply_local_curves_for_mask(index, adjusted);
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

@compute @workgroup_size(8, 8, 1)
fn prepare_scene_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    var rgb = Profile::apply_camera_characterization(scene_working_at(pos));
    let profile_exposure_ev = bitcast<f32>(Common::camera_uniforms.profile_flags.z);
    rgb = rgb * exp2(profile_exposure_ev);
    rgb = BasicAdjustments::apply_exposure(rgb);
    rgb = apply_local_exposure_nodes(pos, rgb);
    textureStore(adjustment_base_out, pos, vec4<f32>(rgb, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_scene_tone_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);

    rgb = DetailCapture::apply_capture_sharpening(pos, rgb);
    rgb = Tonemap::apply_basic_tone(rgb, pos);
    textureStore(local_effects_out, pos, vec4<f32>(rgb, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_local_scene_tone_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);
    rgb = apply_local_scene_tone_nodes(pos, rgb);
    textureStore(local_effects_out, pos, vec4<f32>(rgb, 1.0));
}
