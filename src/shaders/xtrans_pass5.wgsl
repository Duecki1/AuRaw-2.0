// Markesteijn homogeneity-map stage. For each direction, count the 3x3
// derivatives below eight times the minimum center derivative. The following
// accumulation pass performs the reference 5x5 sum over these maps.
@group(0) @binding(20) var mark_drv_0_3_read: texture_2d<f32>;
@group(0) @binding(21) var mark_drv_4_7_read: texture_2d<f32>;
@group(0) @binding(24) var mark_homo_0_3_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(25) var mark_homo_4_7_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

const MARK_HOMO_MARGIN: i32 = 15;

fn mark_drv(pos: vec2<i32>, index: u32) -> f32 {
    let p = clamp_pos(pos);
    if index < 4u {
        return textureLoad(mark_drv_0_3_read, p, 0)[index];
    }
    return textureLoad(mark_drv_4_7_read, p, 0)[index - 4u];
}

fn mark_drv_threshold(pos: vec2<i32>) -> f32 {
    let a = textureLoad(mark_drv_0_3_read, pos, 0);
    let b = textureLoad(mark_drv_4_7_read, pos, 0);
    let minimum = min(
        min(min(a.x, a.y), min(a.z, a.w)),
        min(min(b.x, b.y), min(b.z, b.w)),
    );
    return max(minimum * 8.0, 1e-12);
}

fn mark_local_homogeneity(pos: vec2<i32>, index: u32) -> f32 {
    let threshold = mark_drv_threshold(pos);
    var count = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if mark_drv(pos + vec2<i32>(dx, dy), index) <= threshold {
                count += 1.0;
            }
        }
    }
    return count;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_homogeneity(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let valid = pos.x >= MARK_HOMO_MARGIN && pos.y >= MARK_HOMO_MARGIN
        && pos.x < i32(params.width) - MARK_HOMO_MARGIN
        && pos.y < i32(params.height) - MARK_HOMO_MARGIN;
    if !valid {
        textureStore(mark_homo_0_3_write, pos, vec4<f32>(0.0));
        textureStore(mark_homo_4_7_write, pos, vec4<f32>(0.0));
        return;
    }
    textureStore(mark_homo_0_3_write, pos, vec4<f32>(
        mark_local_homogeneity(pos, 0u),
        mark_local_homogeneity(pos, 1u),
        mark_local_homogeneity(pos, 2u),
        mark_local_homogeneity(pos, 3u),
    ));
    textureStore(mark_homo_4_7_write, pos, vec4<f32>(
        mark_local_homogeneity(pos, 4u),
        mark_local_homogeneity(pos, 5u),
        mark_local_homogeneity(pos, 6u),
        mark_local_homogeneity(pos, 7u),
    ));
}
