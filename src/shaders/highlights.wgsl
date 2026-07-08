struct HighlightParams {
    width: u32,
    height: u32,
    mode: u32,            // 0: Clip, 1: LCh
    cfa_pattern: u32,     // 0: RGGB, 1: BGGR, 2: GRBG, 3: GBRG (ignored for X-Trans)
    clip_threshold: f32,  // Typically 1.0 for normalized RAW data
    is_xtrans: u32,       // 0: Bayer, 1: X-Trans
    roi_x: u32,           // Crop region horizontal offset (relative to sensor top-left)
    roi_y: u32,           // Crop region vertical offset (relative to sensor top-left)
    xtrans_pattern: array<vec4<u32>, 9>, // Packed 6x6 color layout
}

@group(0) @binding(0) var<uniform> params: HighlightParams;
@group(0) @binding(1) var raw_in: texture_2d<f32>;
@group(0) @binding(2) var raw_out: texture_storage_2d<r32float, write>;

const SQRT3: f32 = 1.7320508075;
const SQRT12: f32 = 3.4641016151;

// Identifies the Bayer component of a coordinate
fn get_bayer_color(x: i32, y: i32, cfa_pattern: u32) -> u32 {
    let px = u32(x & 1);
    let py = u32(y & 1);
    let idx = (py << 1u) | px;
    
    // Pattern maps:
    // RGGB (0): [R, G, G, B] -> [0, 1, 1, 2]
    // BGGR (1): [B, G, G, R] -> [2, 1, 1, 0]
    // GRBG (2): [G, R, B, G] -> [1, 0, 2, 1]
    // GBRG (3): [G, B, R, G] -> [1, 2, 0, 1]
    if cfa_pattern == 0u {
        let arr = array<u32, 4>(0u, 1u, 1u, 2u);
        return arr[idx];
    } else if cfa_pattern == 1u {
        let arr = array<u32, 4>(2u, 1u, 1u, 0u);
        return arr[idx];
    } else if cfa_pattern == 2u {
        let arr = array<u32, 4>(1u, 0u, 2u, 1u);
        return arr[idx];
    } else {
        let arr = array<u32, 4>(1u, 2u, 0u, 1u);
        return arr[idx];
    }
}

// Identifies X-Trans component utilizing the 6x6 packed grid
fn get_xtrans_color(row: i32, col: i32, roi_x: i32, roi_y: i32) -> u32 {
    let r = (row + roi_y) % 6;
    let c = (col + roi_x) % 6;
    let r_idx = (r + 6) % 6;
    let c_idx = (c + 6) % 6;
    let idx = r_idx * 6 + c_idx;
    
    let vec_idx = idx / 4;
    let comp_idx = idx % 4;
    let val = params.xtrans_pattern[vec_idx];
    
    if comp_idx == 0 {
        return val.x;
    } else if comp_idx == 1 {
        return val.y;
    } else if comp_idx == 2 {
        return val.z;
    } else {
        return val.w;
    }
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = i32(gid.x);
    let y = i32(gid.y);
    let width = i32(params.width);
    let height = i32(params.height);

    if x >= width || y >= height {
        return;
    }

    let center_val = textureLoad(raw_in, vec2<i32>(x, y), 0).r;

    // Border handling: fallback directly to clipping
    if x <= 1 || x >= width - 2 || y <= 1 || y >= height - 2 {
        textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(min(center_val, params.clip_threshold), 0.0, 0.0, 0.0));
        return;
    }

    if params.mode == 0u {
        // --- Mode 0: Clip Mode ---
        textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(min(center_val, params.clip_threshold), 0.0, 0.0, 0.0));
        return;
    } else {
        // --- Mode 1: LCh Highlight Reconstruction ---
        if params.is_xtrans == 1u {
            // X-Trans LCh reconstruction
            var clipped = (center_val > params.clip_threshold);
            if !clipped {
                // Search local 3x3 for any clipping
                for (var dy = -1; dy <= 1; dy++) {
                    for (var dx = -1; dx <= 1; dx++) {
                        let val = textureLoad(raw_in, vec2<i32>(x + dx, y + dy), 0).r;
                        if val > params.clip_threshold {
                            clipped = true;
                            break;
                        }
                    }
                    if clipped { break; }
                }
            }

            if clipped {
                var mean = vec3<f32>(0.0);
                var RGBmax = vec3<f32>(-1.0e10);
                var cnt = vec3<f32>(0.0);

                // Sample 3x3 neighborhood
                for (var dy = -1; dy <= 1; dy++) {
                    for (var dx = -1; dx <= 1; dx++) {
                        let nx = x + dx;
                        let ny = y + dy;
                        let val = textureLoad(raw_in, vec2<i32>(nx, ny), 0).r;
                        let c = get_xtrans_color(ny, nx, i32(params.roi_x), i32(params.roi_y));
                        
                        if c < 3u {
                            mean[c] += val;
                            cnt[c] += 1.0;
                            RGBmax[c] = max(RGBmax[c], val);
                        }
                    }
                }

                let Ro = min(mean[0] / max(cnt[0], 1.0), params.clip_threshold);
                let Go = min(mean[1] / max(cnt[1], 1.0), params.clip_threshold);
                let Bo = min(mean[2] / max(cnt[2], 1.0), params.clip_threshold);

                let R = RGBmax[0];
                let G = RGBmax[1];
                let B = RGBmax[2];

                let L = (R + G + B) / 3.0;

                var C = SQRT3 * (R - G);
                var H = 2.0 * B - G - R;

                let Co = SQRT3 * (Ro - Go);
                let Ho = 2.0 * Bo - Go - Ro;

                if R != G && G != B {
                    let num = Co * Co + Ho * Ho;
                    let den = C * C + H * H;
                    if den > 1.0e-12 {
                        let ratio = sqrt(num / den);
                        C *= ratio;
                        H *= ratio;
                    }
                }

                let r_val = L - H / 6.0 + C / SQRT12;
                let g_val = L - H / 6.0 - C / SQRT12;
                let b_val = L + H / 3.0;

                let current_color = get_xtrans_color(y, x, i32(params.roi_x), i32(params.roi_y));
                var final_val = center_val;
                if current_color == 0u {
                    final_val = r_val;
                } else if current_color == 1u {
                    final_val = g_val;
                } else if current_color == 2u {
                    final_val = b_val;
                }

                textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(final_val, 0.0, 0.0, 0.0));
            } else {
                textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(center_val, 0.0, 0.0, 0.0));
            }
        } else {
            // Bayer LCh reconstruction
            var clipped = false;
            var R: f32 = 0.0;
            var Gmin: f32 = 1.0e10;
            var Gmax: f32 = -1.0e10;
            var B: f32 = 0.0;

            // Sample sliding 2x2 block anchored at the current pixel
            for (var dy = 0; dy <= 1; dy++) {
                for (var dx = 0; dx <= 1; dx++) {
                    let nx = x + dx;
                    let ny = y + dy;
                    let val = textureLoad(raw_in, vec2<i32>(nx, ny), 0).r;
                    
                    if val > params.clip_threshold {
                        clipped = true;
                    }

                    let c = get_bayer_color(nx, ny, params.cfa_pattern);
                    if c == 0u {
                        R = val;
                    } else if c == 1u {
                        Gmin = min(Gmin, val);
                        Gmax = max(Gmax, val);
                    } else if c == 2u {
                        B = val;
                    }
                }
            }

            if clipped {
                let Ro = min(R, params.clip_threshold);
                let Go = min(Gmin, params.clip_threshold);
                let Bo = min(B, params.clip_threshold);

                let L = (R + Gmax + B) / 3.0;

                var C = SQRT3 * (R - Gmax);
                var H = 2.0 * B - Gmax - R;

                let Co = SQRT3 * (Ro - Go);
                let Ho = 2.0 * Bo - Go - Ro;

                if R != Gmax && Gmax != B {
                    let num = Co * Co + Ho * Ho;
                    let den = C * C + H * H;
                    if den > 1.0e-12 {
                        let ratio = sqrt(num / den);
                        C *= ratio;
                        H *= ratio;
                    }
                }

                let r_val = L - H / 6.0 + C / SQRT12;
                let g_val = L - H / 6.0 - C / SQRT12;
                let b_val = L + H / 3.0;

                let current_color = get_bayer_color(x, y, params.cfa_pattern);
                var final_val = center_val;
                if current_color == 0u {
                    final_val = r_val;
                } else if current_color == 1u {
                    final_val = g_val;
                } else if current_color == 2u {
                    final_val = b_val;
                }

                textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(final_val, 0.0, 0.0, 0.0));
            } else {
                textureStore(raw_out, vec2<i32>(x, y), vec4<f32>(center_val, 0.0, 0.0, 0.0));
            }
        }
    }
}