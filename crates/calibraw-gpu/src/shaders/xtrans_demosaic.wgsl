// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from darktable 5.6.0 Markesteijn X-Trans demosaicing.
// Copyright (C) 2010-2026 darktable developers.
// Markesteijn algorithm credit: Frank Markesteijn (via dcraw and darktable).
// Copyright (C) 2026 CalibRaw contributors (WGSL adaptation).

#import calibraw::common as Common
#import calibraw::xtrans::seed::{xtrans_seed_channel}
#import calibraw::xtrans::markesteijn_interpolate::{mark13_pass1, mark13_pass3}
#import calibraw::xtrans::markesteijn_refine::{mark2_pass2}
#import calibraw::xtrans::markesteijn_candidates::{mark_has_margin}
#import calibraw::xtrans::markesteijn_derivatives::{mark_derivative}
#import calibraw::xtrans::markesteijn_homogeneity::{MARK_HOMO_MARGIN, mark_local_homogeneity}
#import calibraw::xtrans::markesteijn_accumulate::{mark_accumulate}


@group(0) @binding(4) var xtrans_seed_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let red = xtrans_seed_channel(pos, 0u);
    let green = xtrans_seed_channel(pos, 1u);
    let blue = xtrans_seed_channel(pos, 2u);
    let confidence = min(red.y, min(green.y, blue.y));
    textureStore(
        xtrans_seed_write,
        pos,
        vec4<f32>(red.x, green.x, blue.x, confidence),
    );
}

@group(0) @binding(6) var markesteijn_write_13: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass1(pos), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass3(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass3(pos), 1.0));
}

@group(0) @binding(8) var markesteijn_write_2: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_2, pos, vec4<f32>(mark2_pass2(pos), 1.0));
}

@group(0) @binding(20) var mark_drv_0_3_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;
@group(0) @binding(21) var mark_drv_4_7_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_derivatives(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    if !mark_has_margin(pos) {
        textureStore(mark_drv_0_3_write, pos, vec4<f32>(0.0));
        textureStore(mark_drv_4_7_write, pos, vec4<f32>(0.0));
        return;
    }
    textureStore(mark_drv_0_3_write, pos, vec4<f32>(
        mark_derivative(pos, 0u),
        mark_derivative(pos, 1u),
        mark_derivative(pos, 2u),
        mark_derivative(pos, 3u),
    ));
    textureStore(mark_drv_4_7_write, pos, vec4<f32>(
        mark_derivative(pos, 4u),
        mark_derivative(pos, 5u),
        mark_derivative(pos, 6u),
        mark_derivative(pos, 7u),
    ));
}

@group(0) @binding(24) var mark_homo_0_3_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;
@group(0) @binding(25) var mark_homo_4_7_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_homogeneity(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let valid = pos.x >= MARK_HOMO_MARGIN && pos.y >= MARK_HOMO_MARGIN
        && pos.x < i32(Common::camera_uniforms.width) - MARK_HOMO_MARGIN
        && pos.y < i32(Common::camera_uniforms.height) - MARK_HOMO_MARGIN;
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

@group(0) @binding(26) var mark_high_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(mark_high_write, pos, vec4<f32>(mark_accumulate(pos), 1.0));
}
