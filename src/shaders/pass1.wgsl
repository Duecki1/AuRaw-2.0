@group(0) @binding(3) var tex1_write: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn pass1_vh_lpf(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    var v_stat = 0.0;
    var h_stat = 0.0;
    
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        let p = pos + vec2<i32>(0, dy);
        let c  = raw_cfa_at(p);
        let m1 = raw_cfa_at(p + vec2<i32>(0, -1));
        let m2 = raw_cfa_at(p + vec2<i32>(0, -2));
        let m3 = raw_cfa_at(p + vec2<i32>(0, -3));
        let p1 = raw_cfa_at(p + vec2<i32>(0, 1));
        let p2 = raw_cfa_at(p + vec2<i32>(0, 2));
        let p3 = raw_cfa_at(p + vec2<i32>(0, 3));
        let val = m3 - m1 - p1 + p3 - 3.0 * (m2 + p2) + 6.0 * c;
        v_stat += val * val;
    }
    for (var dx = -1; dx <= 1; dx = dx + 1) {
        let p = pos + vec2<i32>(dx, 0);
        let c  = raw_cfa_at(p);
        let m1 = raw_cfa_at(p + vec2<i32>(-1, 0));
        let m2 = raw_cfa_at(p + vec2<i32>(-2, 0));
        let m3 = raw_cfa_at(p + vec2<i32>(-3, 0));
        let p1 = raw_cfa_at(p + vec2<i32>(1, 0));
        let p2 = raw_cfa_at(p + vec2<i32>(2, 0));
        let p3 = raw_cfa_at(p + vec2<i32>(3, 0));
        let val = m3 - m1 - p1 + p3 - 3.0 * (m2 + p2) + 6.0 * c;
        h_stat += val * val;
    }

    let vh_dir = max(1e-10, v_stat) / (max(1e-10, v_stat) + max(1e-10, h_stat));

    var lpf = 0.0;
    if cc != 1u {
        let c  = raw_cfa_at(pos);
        let n  = raw_cfa_at(pos + vec2<i32>(0, -1));
        let s  = raw_cfa_at(pos + vec2<i32>(0, 1));
        let w  = raw_cfa_at(pos + vec2<i32>(-1, 0));
        let e  = raw_cfa_at(pos + vec2<i32>(1, 0));
        let nw = raw_cfa_at(pos + vec2<i32>(-1, -1));
        let ne = raw_cfa_at(pos + vec2<i32>(1, -1));
        let sw = raw_cfa_at(pos + vec2<i32>(-1, 1));
        let se = raw_cfa_at(pos + vec2<i32>(1, 1));
        // Normalized to sum to 1.0
        lpf = 1.0 * c + 0.5 * (n + s + w + e) + 0.25 * (nw + ne + sw + se);
    }

    textureStore(tex1_write, pos, vec4<f32>(vh_dir, lpf, 0.0, 0.0));
}