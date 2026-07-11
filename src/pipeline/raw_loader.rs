#[allow(unused_imports)]
use anyhow::{anyhow, Result};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CfaKind {
    #[default]
    Bayer,
    XTrans,
}

#[derive(Clone, Debug)]
pub struct LoadedRaw {
    pub width: u32,
    pub height: u32,
    pub camera_make: String,
    pub camera_model: String,
    pub cfa_kind: CfaKind,
    pub raw_pixels: Vec<u16>,
    pub color_indices: Vec<u8>,
    pub wb_coeffs: [f32; 4],
    pub cam_to_srgb: [[f32; 4]; 3],
    pub black_levels: [f32; 4],
    /// Effective LibRaw black level for every oriented active-area photosite.
    /// This includes the shared level, per-CFA-plane offsets, and an optional
    /// repeating row/column pattern from `cblack[4..]`.
    pub black_levels_per_pixel: Vec<f32>,
    pub white_levels: [f32; 4],
}

#[cfg(not(libraw_available))]
pub fn load_raw_file(_path: &Path) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(libraw_available)]
pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file(path)
}

#[cfg(libraw_available)]
mod libraw_loader {
    use super::{CfaKind, LoadedRaw};
    use anyhow::{anyhow, Context, Result};
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;
    use std::path::Path;

    // Rec.2020 and the camera profiles used here are D65-referred. Normalizing
    // XYZ -> camera rows against equal-energy XYZ (1, 1, 1) makes an otherwise
    // neutral camera value warm. These coordinates make camera neutral map to
    // the Rec.2020 neutral axis instead.
    const D65_XYZ: [f32; 3] = [0.9504559, 1.0, 1.0890578];
    const XYZ_TO_REC2020: [[f32; 3]; 3] = [
        [1.7166512, -0.3556708, -0.2533663],
        [-0.6666844, 1.6164812, 0.0157685],
        [0.0176399, -0.0427706, 0.9421031],
    ];

    #[allow(
        dead_code,
        non_camel_case_types,
        non_snake_case,
        non_upper_case_globals
    )]
    mod ffi {
        include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
    }

    pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
        let path = CString::new(path.to_string_lossy().as_bytes())
            .context("RAW path contains an interior NUL byte")?;
        let ctx = LibRawContext::new()?;

        check_libraw(
            unsafe { ffi::libraw_open_file(ctx.raw, path.as_ptr()) },
            "open RAW file",
        )?;
        check_libraw(unsafe { ffi::libraw_unpack(ctx.raw) }, "unpack RAW file")?;

        unsafe { loaded_raw_from_context(&ctx) }
    }

    struct LibRawContext {
        raw: *mut ffi::libraw_data_t,
    }

    impl LibRawContext {
        fn new() -> Result<Self> {
            let raw = unsafe { ffi::libraw_init(0) };
            if raw.is_null() {
                Err(anyhow!("libraw_init returned null"))
            } else {
                Ok(Self { raw })
            }
        }
    }

    impl Drop for LibRawContext {
        fn drop(&mut self) {
            unsafe {
                ffi::libraw_close(self.raw);
            }
        }
    }

    unsafe fn loaded_raw_from_context(ctx: &LibRawContext) -> Result<LoadedRaw> {
        let raw = &*ctx.raw;
        let rawdata = &raw.rawdata;
        let sizes = &rawdata.sizes;
        let color = &rawdata.color;
        let iparams = &rawdata.iparams;

        if rawdata.raw_image.is_null() {
            return Err(anyhow!(
                "LibRaw did not expose a single-channel raw_image buffer"
            ));
        }

        let raw_width = sizes.raw_width as u32;
        let raw_height = sizes.raw_height as u32;
        let crop_x = sizes.left_margin as u32;
        let crop_y = sizes.top_margin as u32;
        let width = sizes.width as u32;
        let height = sizes.height as u32;
        if width == 0 || height == 0 {
            return Err(anyhow!("LibRaw reported empty RAW dimensions"));
        }
        if !sizes.pixel_aspect.is_finite() || sizes.pixel_aspect <= 0.0 {
            return Err(anyhow!(
                "LibRaw reported invalid pixel aspect ratio {}",
                sizes.pixel_aspect
            ));
        }
        if (sizes.pixel_aspect - 1.0).abs() > 1e-6 {
            return Err(anyhow!(
                "non-square RAW pixels (aspect {}) require a geometry-resampling stage that AuRaw does not implement yet",
                sizes.pixel_aspect
            ));
        }
        if !matches!(sizes.flip, 0 | 3 | 5 | 6) {
            return Err(anyhow!(
                "unsupported LibRaw orientation code {}; expected 0, 3, 5, or 6",
                sizes.flip
            ));
        }

        let crop_right = crop_x
            .checked_add(width)
            .ok_or_else(|| anyhow!("LibRaw horizontal crop overflow"))?;
        let crop_bottom = crop_y
            .checked_add(height)
            .ok_or_else(|| anyhow!("LibRaw vertical crop overflow"))?;
        if crop_right > raw_width || crop_bottom > raw_height {
            return Err(anyhow!(
                "LibRaw crop is outside RAW bounds: crop {}x{} at {},{} in {}x{}",
                width,
                height,
                crop_x,
                crop_y,
                raw_width,
                raw_height
            ));
        }

        let cfa_kind = cfa_kind_from_filters(iparams.filters)?;
        let cdesc = cdesc4(iparams);
        let cfa_map = canonical_cfa_map(cdesc)?;
        let physical_black_levels = black_levels(color.black, &color.cblack);
        let (width, height, raw_pixels, color_indices, black_levels_per_pixel) = copy_active_pixels(
            ctx.raw,
            rawdata.raw_image,
            raw_width,
            raw_height,
            crop_x,
            crop_y,
            width,
            height,
            sizes.raw_pitch as usize,
            sizes.flip,
            cdesc,
            cfa_map,
            color.black,
            &color.cblack,
        )?;
        let physical_wb = white_balance(color.cam_mul, cdesc);
        let wb_coeffs = canonicalize_f32x4(physical_wb, cfa_map);
        let cam_to_srgb = camera_to_working_matrix(color, physical_wb, cdesc)?;
        let black_levels = canonicalize_f32x4(physical_black_levels, cfa_map);
        // LibRaw changed `linear_max` from `long[4]` in the 0.21 series
        // to `unsigned[4]` in newer releases. Bindgen therefore exposes it
        // as either `[i64; 4]` or `[u32; 4]`, depending on the installed
        // headers. Normalize both representations and reject negative or
        // otherwise out-of-range metadata values.
        let linear_max = color
            .linear_max
            .map(|value| u32::try_from(value).unwrap_or(0));
        let white_levels = canonicalize_f32x4(
            white_levels(color.maximum, linear_max, physical_black_levels),
            cfa_map,
        );

        Ok(LoadedRaw {
            width,
            height,
            camera_make: c_array_to_string(&iparams.make),
            camera_model: c_array_to_string(&iparams.model),
            cfa_kind,
            raw_pixels,
            color_indices,
            wb_coeffs,
            cam_to_srgb,
            black_levels,
            black_levels_per_pixel,
            white_levels,
        })
    }

    unsafe fn copy_active_pixels(
        raw: *mut ffi::libraw_data_t,
        raw_image: *const u16,
        raw_width: u32,
        raw_height: u32,
        crop_x: u32,
        crop_y: u32,
        width: u32,
        height: u32,
        raw_pitch: usize,
        flip: i32,
        cdesc: [u8; 4],
        cfa_map: [u8; 4],
        shared_black: u32,
        cblack: &[u32],
    ) -> Result<(u32, u32, Vec<u16>, Vec<u8>, Vec<f32>)> {
        let raw_width = raw_width as usize;
        let raw_height = raw_height as usize;
        let crop_x = crop_x as usize;
        let crop_y = crop_y as usize;
        let width = width as usize;
        let height = height as usize;
        let row_bytes = raw_width
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or_else(|| anyhow!("RAW row size overflow"))?;
        let pitch = if raw_pitch == 0 { row_bytes } else { raw_pitch };
        if pitch < row_bytes {
            return Err(anyhow!(
                "LibRaw raw_pitch ({pitch}) is smaller than one decoded row ({row_bytes})"
            ));
        }
        if pitch % std::mem::align_of::<u16>() != 0 {
            return Err(anyhow!("LibRaw raw_pitch ({pitch}) is not u16-aligned"));
        }

        let crop_right = crop_x
            .checked_add(width)
            .ok_or_else(|| anyhow!("active RAW horizontal crop overflow"))?;
        let crop_bottom = crop_y
            .checked_add(height)
            .ok_or_else(|| anyhow!("active RAW vertical crop overflow"))?;
        if crop_bottom > raw_height || crop_right > raw_width {
            return Err(anyhow!("active RAW crop exceeds decoded RAW buffer"));
        }

        let (out_width, out_height) = match flip {
            5 | 6 => (height, width),
            _ => (width, height),
        };
        let output_len = out_width
            .checked_mul(out_height)
            .ok_or_else(|| anyhow!("oriented RAW dimensions overflow"))?;
        let mut pixels = vec![0; output_len];
        let mut colors = Vec::with_capacity(output_len);
        let mut black_map = Vec::with_capacity(output_len);

        for y in 0..out_height {
            for x in 0..out_width {
                let (src_x, src_y) = oriented_source_pos(x, y, width, height, flip);
                let raw_x = crop_x + src_x;
                let raw_y = crop_y + src_y;
                let row_offset = raw_y
                    .checked_mul(pitch)
                    .ok_or_else(|| anyhow!("RAW row pointer offset overflow"))?;
                let row_ptr = (raw_image as *const u8).add(row_offset) as *const u16;
                pixels[y * out_width + x] = *row_ptr.add(raw_x);

                let libraw_color = ffi::libraw_COLOR(raw, raw_y as i32, raw_x as i32);
                if !(0..=3).contains(&libraw_color) {
                    return Err(anyhow!(
                        "LibRaw returned invalid CFA channel {libraw_color} at {raw_x},{raw_y}"
                    ));
                }
                if cdesc[libraw_color as usize] == 0 {
                    return Err(anyhow!(
                        "LibRaw used undescribed CFA channel {libraw_color} at {raw_x},{raw_y}"
                    ));
                }
                // Preserve four independent CFA planes, but canonicalize
                // them to R, G1, B, G2 so the GPU can keep G1/G2 calibration
                // separate without carrying a second channel-map uniform.
                colors.push(cfa_map[libraw_color as usize]);
                black_map.push(effective_black_level(
                    shared_black,
                    cblack,
                    libraw_color as usize,
                    src_x,
                    src_y,
                ));
            }
        }

        Ok((
            out_width as u32,
            out_height as u32,
            pixels,
            colors,
            black_map,
        ))
    }

    fn oriented_source_pos(
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        flip: i32,
    ) -> (usize, usize) {
        match flip {
            3 => (width - 1 - x, height - 1 - y),
            5 => (width - 1 - y, x),
            6 => (y, height - 1 - x),
            _ => (x, y),
        }
    }

    fn cdesc4(iparams: &ffi::libraw_iparams_t) -> [u8; 4] {
        [
            iparams.cdesc[0] as u8,
            iparams.cdesc[1] as u8,
            iparams.cdesc[2] as u8,
            iparams.cdesc[3] as u8,
        ]
    }

    fn cfa_kind_from_filters(filters: u32) -> Result<CfaKind> {
        match filters {
            // LibRaw reserves 9 for the Fuji 6x6 X-Trans matrix.
            9 => Ok(CfaKind::XTrans),
            // Ordinary Bayer masks use the packed 32-bit representation.
            value if value >= 1000 => Ok(CfaKind::Bayer),
            0 => Err(anyhow!(
                "full-colour/linear RAW input is not supported by the CFA GPU pipeline"
            )),
            1 => Err(anyhow!(
                "Leaf CatchLight 16x16 CFA is not supported by the current demosaic paths"
            )),
            value => Err(anyhow!(
                "unsupported LibRaw CFA filter code {value}; expected Bayer or Fuji X-Trans"
            )),
        }
    }

    fn canonical_cfa_map(cdesc: [u8; 4]) -> Result<[u8; 4]> {
        let mut map = [3u8; 4];
        let mut red_count = 0u8;
        let mut green_count = 0u8;
        let mut blue_count = 0u8;

        for index in 0..4 {
            map[index] = match cdesc[index] as char {
                'R' | 'r' => {
                    red_count = red_count.saturating_add(1);
                    0
                }
                'B' | 'b' => {
                    blue_count = blue_count.saturating_add(1);
                    2
                }
                'G' | 'g' => {
                    let canonical = if green_count == 0 { 1 } else { 3 };
                    green_count = green_count.saturating_add(1);
                    canonical
                }
                '\0' => 3,
                other => {
                    return Err(anyhow!(
                        "unsupported non-RGB CFA descriptor {other:?} in {:?}",
                        cdesc.map(char::from)
                    ));
                }
            };
        }

        if red_count != 1 || blue_count != 1 || !(1..=2).contains(&green_count) {
            return Err(anyhow!(
                "unsupported RGB CFA descriptor {:?}; expected one red, one blue, and one or two green planes",
                cdesc.map(char::from)
            ));
        }

        Ok(map)
    }

    fn canonicalize_f32x4(values: [f32; 4], cfa_map: [u8; 4]) -> [f32; 4] {
        let mut out = [0.0; 4];
        for physical in 0..4 {
            out[cfa_map[physical] as usize] = values[physical];
        }
        out
    }

    fn logical_rgb_channel(cdesc: [u8; 4], cfa_channel: usize) -> Option<usize> {
        match cdesc[cfa_channel.min(3)] as char {
            'R' | 'r' => Some(0),
            'G' | 'g' => Some(1),
            'B' | 'b' => Some(2),
            // A NUL descriptor marks an unused physical profile row. Do not
            // fold it into a real RGB channel, even if malformed metadata left
            // non-zero coefficients there.
            _ => None,
        }
    }

    fn white_balance(mut wb: [f32; 4], cdesc: [u8; 4]) -> [f32; 4] {
        let mut green_sum = 0.0;
        let mut green_count = 0.0;

        for index in 0..4 {
            let is_green = matches!(cdesc[index] as char, 'G' | 'g');
            if is_green && wb[index].is_finite() && wb[index] > 0.0 {
                green_sum += wb[index];
                green_count += 1.0;
            }
        }

        let green_reference = if green_count > 0.0 {
            green_sum / green_count
        } else if wb[1].is_finite() && wb[1] > 0.0 {
            wb[1]
        } else {
            1.0
        };

        for value in &mut wb {
            *value = if value.is_finite() && *value > 0.0 {
                *value / green_reference
            } else {
                1.0
            };
        }

        wb
    }

    fn black_levels(black: u32, cblack: &[u32]) -> [f32; 4] {
        let mut out = [black as f32; 4];
        for (index, value) in out.iter_mut().enumerate() {
            *value += cblack.get(index).copied().unwrap_or(0) as f32;
        }
        out
    }

    fn effective_black_level(
        black: u32,
        cblack: &[u32],
        channel: usize,
        active_x: usize,
        active_y: usize,
    ) -> f32 {
        let channel_offset = cblack.get(channel.min(3)).copied().unwrap_or(0);
        let pattern_offset = black_pattern_dimensions(cblack)
            .and_then(|(rows, cols)| {
                let pattern_index = (active_y % rows)
                    .checked_mul(cols)?
                    .checked_add(active_x % cols)?
                    .checked_add(6)?;
                cblack.get(pattern_index).copied()
            })
            .unwrap_or(0);

        black
            .saturating_add(channel_offset)
            .saturating_add(pattern_offset) as f32
    }

    fn black_pattern_dimensions(cblack: &[u32]) -> Option<(usize, usize)> {
        let rows = usize::try_from(*cblack.get(4)?).ok()?;
        let cols = usize::try_from(*cblack.get(5)?).ok()?;
        if rows == 0 || cols == 0 {
            return None;
        }
        let values = rows.checked_mul(cols)?;
        let end = 6usize.checked_add(values)?;
        (end <= cblack.len()).then_some((rows, cols))
    }

    fn white_levels(
        maximum: u32,
        linear_max: [u32; 4],
        black_levels: [f32; 4],
    ) -> [f32; 4] {
        // `maximum` is LibRaw's decoded white/saturation level. `linear_max`
        // is an optional per-plane vendor "specular white" / linearity limit
        // and is known to be invalid in some files. Use it only when it forms
        // a sane range and does not exceed a reported shared maximum.
        let shared_fallback = (maximum != 0)
            .then_some(maximum)
            .or_else(|| linear_max.iter().copied().find(|value| *value != 0))
            .unwrap_or(65535);

        let mut out = [shared_fallback as f32; 4];
        for index in 0..4 {
            let candidate = linear_max[index];
            let candidate_is_sane = candidate != 0
                && candidate as f32 > black_levels[index] + 1.0
                && (maximum == 0 || candidate <= maximum);
            if candidate_is_sane {
                out[index] = candidate as f32;
            }
        }
        out
    }

    fn cam_to_working(xyz_to_cam: [[f32; 3]; 4], cdesc: [u8; 4]) -> [[f32; 4]; 3] {
        let cam_to_xyz = normalized_pseudoinverse(xyz_to_cam);

        let mut physical = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                physical[row][col] = XYZ_TO_REC2020[row][0] * cam_to_xyz[0][col]
                    + XYZ_TO_REC2020[row][1] * cam_to_xyz[1][col]
                    + XYZ_TO_REC2020[row][2] * cam_to_xyz[2][col];
            }
        }

        // The demosaic output is RGB, but camera profiles can contain four
        // physical planes (normally R, G1, B, G2). Fold profile columns by
        // cdesc only after each CFA plane has been normalized independently.
        let mut out = [[0.0; 4]; 3];
        for physical_col in 0..4 {
            let Some(rgb_col) = logical_rgb_channel(cdesc, physical_col) else {
                continue;
            };
            for row in 0..3 {
                out[row][rgb_col] += physical[row][physical_col];
            }
        }

        out
    }

    fn camera_to_working_matrix(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
    ) -> Result<[[f32; 4]; 3]> {
        let matrix = if let Some(xyz_to_cam) = interpolated_dng_xyz_to_cam(color, wb_coeffs, cdesc) {
            cam_to_working(xyz_to_cam, cdesc)
        } else {
            cam_to_working(color.cam_xyz, cdesc)
        };

        if matrix.iter().flatten().any(|value| !value.is_finite())
            || matrix.iter().flatten().all(|value| value.abs() <= 1e-12)
        {
            return Err(anyhow!(
                "LibRaw did not provide an invertible camera colour matrix; refusing to treat camera RGB as the working colour space"
            ));
        }
        Ok(matrix)
    }

    fn interpolated_dng_xyz_to_cam(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
    ) -> Option<[[f32; 3]; 4]> {
        let matrix0 = calibrated_dng_xyz_to_cam(&color.dng_color[0])?;
        let matrix1 = calibrated_dng_xyz_to_cam(&color.dng_color[1])?;
        let cct0 = calibration_illuminant_cct(color.dng_color[0].illuminant)?;
        let cct1 = calibration_illuminant_cct(color.dng_color[1].illuminant)?;
        let scene_cct = estimate_scene_cct(color, wb_coeffs, cdesc)?;

        let mired0 = 1_000_000.0 / cct0;
        let mired1 = 1_000_000.0 / cct1;
        let mired = 1_000_000.0 / scene_cct;
        let denom = mired1 - mired0;
        if denom.abs() < 1e-6 {
            return None;
        }

        let t = ((mired - mired0) / denom).clamp(0.0, 1.0);
        let mut out = [[0.0; 3]; 4];
        for row in 0..4 {
            for col in 0..3 {
                out[row][col] = matrix0[row][col] * (1.0 - t) + matrix1[row][col] * t;
            }
        }
        Some(out)
    }

    fn calibrated_dng_xyz_to_cam(dng: &ffi::libraw_dng_color_t) -> Option<[[f32; 3]; 4]> {
        if !matrix4x3_is_valid(dng.colormatrix) {
            return None;
        }

        let calibration = identity_fallback_4x4(dng.calibration);
        let mut out = [[0.0; 3]; 4];
        for row in 0..4 {
            for col in 0..3 {
                for k in 0..4 {
                    out[row][col] += calibration[row][k] * dng.colormatrix[k][col];
                }
            }
        }

        if matrix4x3_is_valid(out) {
            Some(out)
        } else {
            None
        }
    }

    fn identity_fallback_4x4(matrix: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
        if matrix
            .iter()
            .flatten()
            .any(|v| v.is_finite() && v.abs() > 1e-8)
        {
            matrix
        } else {
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        }
    }

    fn matrix4x3_is_valid(matrix: [[f32; 3]; 4]) -> bool {
        matrix.iter().flatten().all(|v| v.is_finite())
            && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
    }

    fn estimate_scene_cct(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
    ) -> Option<f32> {
        let mut best_cct = 0.0;
        let mut best_error = f32::INFINITY;

        for row in color.WBCT_Coeffs {
            let cct = row[0];
            if !cct.is_finite() || cct <= 0.0 {
                continue;
            }

            let candidate = white_balance([row[1], row[2], row[3], row[4]], cdesc);
            let error = (candidate[0].ln() - wb_coeffs[0].ln()).abs()
                + (candidate[2].ln() - wb_coeffs[2].ln()).abs();

            if error < best_error {
                best_error = error;
                best_cct = cct;
            }
        }

        if best_cct > 0.0 {
            Some(best_cct.clamp(1500.0, 50000.0))
        } else {
            None
        }
    }

    fn calibration_illuminant_cct(illuminant: u16) -> Option<f32> {
        match illuminant {
            17 => Some(2856.0),
            18 => Some(4874.0),
            19 => Some(6774.0),
            20 => Some(5503.0),
            21 => Some(6504.0),
            22 => Some(7504.0),
            23 => Some(5003.0),
            24 => Some(3200.0),
            _ => None,
        }
    }

    fn normalized_pseudoinverse(mut xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
        for row in &mut xyz_to_cam {
            // Match Ansel/dcraw's normalization of XYZ -> camera after the
            // sRGB/XYZ D65 matrix has been applied: each camera row must
            // produce one for the D65 white point, not for equal-energy XYZ.
            let white_response = row[0] * D65_XYZ[0] + row[1] * D65_XYZ[1] + row[2] * D65_XYZ[2];
            if white_response.is_finite() && white_response.abs() > 1e-12 {
                for value in row {
                    *value /= white_response;
                }
            }
        }

        pseudoinverse(xyz_to_cam)
    }

    fn pseudoinverse(input: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
        // Form (A^T A | I) in f64. Camera matrices are small, but doing the
        // inversion in f32 makes near-dependent profile columns needlessly
        // fragile and can silently force the identity colour fallback.
        let mut temp = [[0.0f64; 6]; 3];

        for i in 0..3 {
            temp[i][i + 3] = 1.0;
            for j in 0..3 {
                for row in &input {
                    temp[i][j] += f64::from(row[i]) * f64::from(row[j]);
                }
            }
        }

        for i in 0..3 {
            let mut pivot_row = i;
            let mut pivot_abs = temp[i][i].abs();
            for (row, values) in temp.iter().enumerate().skip(i + 1) {
                let candidate = values[i].abs();
                if candidate > pivot_abs {
                    pivot_abs = candidate;
                    pivot_row = row;
                }
            }
            if !pivot_abs.is_finite() || pivot_abs < 1e-14 {
                return [[0.0; 4]; 3];
            }
            if pivot_row != i {
                temp.swap(i, pivot_row);
            }

            let pivot = temp[i][i];
            for value in &mut temp[i] {
                *value /= pivot;
            }
            for k in 0..3 {
                if k == i {
                    continue;
                }
                let scale = temp[k][i];
                for j in 0..6 {
                    temp[k][j] -= temp[i][j] * scale;
                }
            }
        }

        let mut out = [[0.0; 4]; 3];
        for col in 0..4 {
            for row in 0..3 {
                let value = (0..3)
                    .map(|k| temp[row][k + 3] * f64::from(input[col][k]))
                    .sum::<f64>();
                if !value.is_finite() {
                    return [[0.0; 4]; 3];
                }
                out[row][col] = value as f32;
            }
        }
        out
    }

    fn c_array_to_string(value: &[c_char]) -> String {
        // Fixed-size LibRaw arrays are normally NUL terminated, but treating
        // them as an unbounded C string is undefined behaviour when malformed
        // metadata fills the entire array. Keep conversion inside the slice.
        let bytes: Vec<u8> = value
            .iter()
            .copied()
            .take_while(|value| *value != 0)
            .map(|value| value as u8)
            .collect();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }

    fn check_libraw(err: i32, action: &str) -> Result<()> {
        if err == 0 {
            return Ok(());
        }

        let message = unsafe {
            let ptr = ffi::libraw_strerror(err);
            if ptr.is_null() {
                "unknown LibRaw error".into()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };

        Err(anyhow!("LibRaw failed to {action}: {message} ({err})"))
    }

    #[cfg(test)]
    mod tests {
        use super::{
            black_levels, cam_to_working, canonical_cfa_map, canonicalize_f32x4,
            cfa_kind_from_filters, effective_black_level, oriented_source_pos, white_balance,
            white_levels, CfaKind,
        };

        const RGBG: [u8; 4] = *b"RGBG";

        #[test]
        fn libraw_filter_codes_select_the_demosaic_family() {
            assert_eq!(cfa_kind_from_filters(9).unwrap(), CfaKind::XTrans);
            assert_eq!(cfa_kind_from_filters(0x9494_9494).unwrap(), CfaKind::Bayer);
            assert!(cfa_kind_from_filters(0).is_err());
            assert!(cfa_kind_from_filters(1).is_err());
        }

        #[test]
        fn documented_libraw_rotations_map_output_to_source_coordinates() {
            // Source is 3x2. A 90-degree output is 2x3.
            assert_eq!(oriented_source_pos(0, 0, 3, 2, 5), (2, 0));
            assert_eq!(oriented_source_pos(1, 2, 3, 2, 5), (0, 1));
            assert_eq!(oriented_source_pos(0, 0, 3, 2, 6), (0, 1));
            assert_eq!(oriented_source_pos(1, 2, 3, 2, 6), (2, 0));
        }

        #[test]
        fn camera_neutral_maps_to_rec2020_neutral() {
            // Identity is a useful synthetic XYZ -> camera profile: the old
            // row-sum normalization mapped camera (1, 1, 1) to equal-energy
            // XYZ and therefore to a visibly warm Rec.2020 value.
            let matrix = cam_to_working(
                [
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0],
                ],
                RGBG,
            );

            for (channel, row) in matrix.iter().enumerate() {
                let mapped_neutral = row[0] + row[1] + row[2];
                assert!(
                    (mapped_neutral - 1.0).abs() < 1e-5,
                    "camera neutral mapped to {mapped_neutral} in working channel {channel}"
                );
            }
        }

        #[test]
        fn cfa_planes_are_canonicalized_without_merging_greens() {
            let map = canonical_cfa_map(*b"GRGB").unwrap();
            assert_eq!(map, [1, 0, 3, 2]);
            assert_eq!(
                canonicalize_f32x4([10.0, 20.0, 30.0, 40.0], map),
                [20.0, 10.0, 40.0, 30.0]
            );
        }

        #[test]
        fn non_rgb_cfa_is_rejected_instead_of_silently_miscolored() {
            assert!(canonical_cfa_map(*b"GMCY").is_err());
            assert!(canonical_cfa_map(*b"RGBG").is_ok());
        }

        #[test]
        fn calibration_keeps_both_green_planes_distinct() {
            assert_eq!(black_levels(64, &[1, 2, 3, 4]), [65.0, 66.0, 67.0, 68.0]);
            assert_eq!(
                white_levels(4095, [4000, 4010, 4020, 4030], [64.0; 4]),
                [4000.0, 4010.0, 4020.0, 4030.0]
            );
        }

        #[test]
        fn invalid_linear_max_falls_back_to_decoded_white_level() {
            assert_eq!(
                white_levels(4095, [10, 4000, 5000, 0], [64.0; 4]),
                [4095.0, 4000.0, 4095.0, 4095.0]
            );
        }

        #[test]
        fn repeating_black_pattern_uses_active_area_coordinates() {
            // Two rows by three columns, after the four per-plane offsets.
            let cblack = [1, 2, 3, 4, 2, 3, 10, 20, 30, 40, 50, 60];
            assert_eq!(effective_black_level(64, &cblack, 2, 0, 0), 77.0);
            assert_eq!(effective_black_level(64, &cblack, 2, 4, 3), 117.0);
        }

        #[test]
        fn malformed_black_pattern_is_ignored_without_out_of_bounds_access() {
            let cblack = [1, 2, 3, 4, 99, 99];
            assert_eq!(effective_black_level(64, &cblack, 1, 500, 500), 66.0);
        }

        #[test]
        fn white_balance_uses_the_average_green_reference() {
            let wb = white_balance([2.0, 1.0, 1.5, 1.2], RGBG);
            let green_mean = 0.5 * (wb[1] + wb[3]);
            assert!((green_mean - 1.0).abs() < 1e-6);
            assert!((wb[1] - wb[3]).abs() > 1e-3);
        }
    }
}
