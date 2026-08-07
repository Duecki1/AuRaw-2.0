#import auraw::common as Common
#import auraw::raw_sampling as RawSampling

// Bayer RCD stage 3: interpolate the opposite red/blue channel at red/blue
// photosites from diagonal colour differences and the P/Q discriminator.
@group(0) @binding(7) var tex2_read: texture_2d<f32>;
@group(0) @binding(8) var tex3_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn rcd_green_at(pos: vec2<i32>) -> f32 {
    return textureLoad(tex2_read, Common::clamp_pos(pos), 0).x;
}

fn rcd_diagonal_difference(pos: vec2<i32>) -> f32 {
    let eps = 1e-5;
    let nw = pos + vec2<i32>(-1, -1);
    let ne = pos + vec2<i32>( 1, -1);
    let sw = pos + vec2<i32>(-1,  1);
    let se = pos + vec2<i32>( 1,  1);
    let nw2 = pos + vec2<i32>(-2, -2);
    let ne2 = pos + vec2<i32>( 2, -2);
    let sw2 = pos + vec2<i32>(-2,  2);
    let se2 = pos + vec2<i32>( 2,  2);
    let nw3 = pos + vec2<i32>(-3, -3);
    let ne3 = pos + vec2<i32>( 3, -3);
    let sw3 = pos + vec2<i32>(-3,  3);
    let se3 = pos + vec2<i32>( 3,  3);

    let nw_grad = eps + abs(RawSampling::raw_cfa_at(nw) - RawSampling::raw_cfa_at(se))
        + abs(RawSampling::raw_cfa_at(nw) - RawSampling::raw_cfa_at(nw3))
        + abs(rcd_green_at(pos) - rcd_green_at(nw2));
    let ne_grad = eps + abs(RawSampling::raw_cfa_at(ne) - RawSampling::raw_cfa_at(sw))
        + abs(RawSampling::raw_cfa_at(ne) - RawSampling::raw_cfa_at(ne3))
        + abs(rcd_green_at(pos) - rcd_green_at(ne2));
    let sw_grad = eps + abs(RawSampling::raw_cfa_at(ne) - RawSampling::raw_cfa_at(sw))
        + abs(RawSampling::raw_cfa_at(sw) - RawSampling::raw_cfa_at(sw3))
        + abs(rcd_green_at(pos) - rcd_green_at(sw2));
    let se_grad = eps + abs(RawSampling::raw_cfa_at(nw) - RawSampling::raw_cfa_at(se))
        + abs(RawSampling::raw_cfa_at(se) - RawSampling::raw_cfa_at(se3))
        + abs(rcd_green_at(pos) - rcd_green_at(se2));

    let nw_est = RawSampling::raw_cfa_at(nw) - rcd_green_at(nw);
    let ne_est = RawSampling::raw_cfa_at(ne) - rcd_green_at(ne);
    let sw_est = RawSampling::raw_cfa_at(sw) - rcd_green_at(sw);
    let se_est = RawSampling::raw_cfa_at(se) - rcd_green_at(se);
    let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
    let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);

    let pq_center = textureLoad(tex2_read, pos, 0).z;
    let pq_neighbours = 0.25 * (
        textureLoad(tex2_read, Common::clamp_pos(nw), 0).z
      + textureLoad(tex2_read, Common::clamp_pos(ne), 0).z
      + textureLoad(tex2_read, Common::clamp_pos(sw), 0).z
      + textureLoad(tex2_read, Common::clamp_pos(se), 0).z
    );
    let pq = clamp(select(pq_center, pq_neighbours,
        abs(0.5 - pq_center) < abs(0.5 - pq_neighbours)), 0.0, 1.0);
    return mix(p_est, q_est, pq);
}

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_chroma(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = RawSampling::color_at(pos);
    let green = rcd_green_at(pos);
    var rgb = vec3<f32>(0.0, green, 0.0);

    if cc == 0u {
        rgb.r = RawSampling::raw_cfa_at(pos);
        rgb.b = green + rcd_diagonal_difference(pos);
    } else if cc == 2u {
        rgb.b = RawSampling::raw_cfa_at(pos);
        rgb.r = green + rcd_diagonal_difference(pos);
    }

    textureStore(tex3_write, pos, vec4<f32>(rgb, textureLoad(tex2_read, pos, 0).w));
}
