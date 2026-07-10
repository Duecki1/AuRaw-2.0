fn sensor_clip_level() -> f32 {
    return 0.995 * max(1.0 + params.clip, 0.05);
}

fn reconstruct_sensor_highlights(rgb: vec3<f32>, clip_mask: f32) -> vec3<f32> {
    let wb = params.wb.xyz;
    let sensor_rgb = rgb / max(wb, vec3<f32>(1e-8));
    let clip = sensor_clip_level();
    let near_clip3 = smoothstep(vec3<f32>(0.90 * clip), vec3<f32>(clip), sensor_rgb);

    let mask = floor(clip_mask + 0.5);
    let r_clipped = mask - 10.0 * floor(mask / 10.0) >= 1.0;
    let g_digit = floor(mask / 10.0) - 10.0 * floor(mask / 100.0);
    let g_clipped = g_digit >= 1.0;
    let b_clipped = floor(mask / 100.0) >= 1.0;

    let near_clip = max(near_clip3, vec3<f32>(
        select(0.0, 1.0, r_clipped),
        select(0.0, 1.0, g_clipped),
        select(0.0, 1.0, b_clipped),
    ));
    let near_count = near_clip.r + near_clip.g + near_clip.b;

    if (near_count < 1e-4) {
        return rgb;
    }

    // Ansel LCH (YCbCr Rec709) based highlight recreation
    let Y = dot(rgb, LUMA);
    let Cb = dot(rgb, vec3<f32>(-0.114572, -0.385428, 0.5));
    let Cr = dot(rgb, vec3<f32>(0.5, -0.454153, -0.045847));
    
    let chroma = sqrt(Cb * Cb + Cr * Cr);
    let saturation = chroma / max(Y, 1e-6);
    let low_saturation = 1.0 - smoothstep(0.08, 0.30, saturation);

    let multi_near = smoothstep(1.25, 2.0, near_count);
    let all_near = smoothstep(2.35, 3.0, near_count);
    let strength = max(multi_near, all_near) * low_saturation;

    let safe_rgb = min(rgb, clip * wb);
    let safe_Cb = dot(safe_rgb, vec3<f32>(-0.114572, -0.385428, 0.5));
    let safe_Cr = dot(safe_rgb, vec3<f32>(0.5, -0.454153, -0.045847));

    let denom = chroma * chroma;
    var new_Cb = Cb;
    var new_Cr = Cr;

    if (denom > 1e-12) {
        let safe_chroma = sqrt(safe_Cb * safe_Cb + safe_Cr * safe_Cr);
        let ratio = min(1.0, safe_chroma / chroma);
        new_Cb *= ratio;
        new_Cr *= ratio;
    }

    let r_out = Y + 1.5748 * new_Cr;
    let g_out = Y - 0.1873 * new_Cb - 0.4681 * new_Cr;
    let b_out = Y + 1.8556 * new_Cb;

    let chroma_limited = vec3<f32>(r_out, g_out, b_out);
    let neutral = vec3<f32>(Y);
    let reconstructed = mix(chroma_limited, neutral, strength);

    let blend = clamp(near_count / 3.0, 0.0, 1.0);
    return max(mix(rgb, reconstructed, blend), vec3<f32>(0.0));
}


