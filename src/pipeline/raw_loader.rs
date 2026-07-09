#[allow(unused_imports)]
use anyhow::{anyhow, Result};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct LoadedRaw {
    pub width: u32,
    pub height: u32,
    pub camera_make: String,
    pub camera_model: String,
    pub raw_pixels: Vec<u16>,
    pub color_indices: Vec<u8>,
    pub wb_coeffs: [f32; 4],
    pub cam_to_srgb: [[f32; 4]; 3],
    pub black_levels: [f32; 4],
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
    use super::LoadedRaw;
    use anyhow::{anyhow, Context, Result};
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;
    use std::path::Path;

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

        if crop_x + width > raw_width || crop_y + height > raw_height {
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

        let (width, height, raw_pixels, color_indices) = copy_active_pixels(
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
            iparams,
        )?;
        let wb_coeffs = white_balance(color.cam_mul);
        let cam_to_srgb = camera_to_working_matrix(color, wb_coeffs);
        let black_levels = black_levels(color.black, &color.cblack, iparams);
        let white_levels = white_levels(color.maximum);

        Ok(LoadedRaw {
            width,
            height,
            camera_make: c_array_to_string(&iparams.make),
            camera_model: c_array_to_string(&iparams.model),
            raw_pixels,
            color_indices,
            wb_coeffs,
            cam_to_srgb,
            black_levels,
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
        iparams: &ffi::libraw_iparams_t,
    ) -> Result<(u32, u32, Vec<u16>, Vec<u8>)> {
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

        if crop_y + height > raw_height || crop_x + width > raw_width {
            return Err(anyhow!("active RAW crop exceeds decoded RAW buffer"));
        }

        let (out_width, out_height) = match flip {
            5 | 6 => (height, width),
            _ => (width, height),
        };
        let mut pixels = vec![0; out_width * out_height];
        let mut colors = Vec::with_capacity(out_width * out_height);

        for y in 0..out_height {
            for x in 0..out_width {
                let (src_x, src_y) = oriented_source_pos(x, y, width, height, flip);
                let raw_x = crop_x + src_x;
                let raw_y = crop_y + src_y;
                let row_ptr = (raw_image as *const u8).add(raw_y * pitch) as *const u16;
                pixels[y * out_width + x] = *row_ptr.add(raw_x);

                let libraw_color = ffi::libraw_COLOR(raw, raw_y as i32, raw_x as i32);
                colors.push(rgb_channel_for_libraw_color(iparams, libraw_color));
            }
        }

        Ok((out_width as u32, out_height as u32, pixels, colors))
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

    fn rgb_channel_for_libraw_color(iparams: &ffi::libraw_iparams_t, color: i32) -> u8 {
        let index = color.clamp(0, 3) as usize;
        match iparams.cdesc[index] as u8 as char {
            'R' | 'r' => 0,
            'G' | 'g' => 1,
            'B' | 'b' => 2,
            _ => color.clamp(0, 2) as u8,
        }
    }

    fn white_balance(mut wb: [f32; 4]) -> [f32; 4] {
        let green = if wb[1].is_finite() && wb[1] > 0.0 {
            wb[1]
        } else {
            1.0
        };

        for v in &mut wb {
            *v = if v.is_finite() && *v > 0.0 {
                *v / green
            } else {
                1.0
            };
        }

        wb
    }

    fn black_levels(black: u32, cblack: &[u32], iparams: &ffi::libraw_iparams_t) -> [f32; 4] {
        let mut sums = [0.0; 4];
        let mut counts = [0.0; 4];

        for index in 0..4 {
            let channel = rgb_channel_for_libraw_color(iparams, index as i32) as usize;
            sums[channel] += black as f32 + cblack.get(index).copied().unwrap_or(0) as f32;
            counts[channel] += 1.0;
        }

        let mut out = [black as f32; 4];
        for channel in 0..3 {
            if counts[channel] > 0.0 {
                out[channel] = sums[channel] / counts[channel];
            }
        }
        out
    }

    fn white_levels(maximum: u32) -> [f32; 4] {
        let white = if maximum == 0 {
            65535.0
        } else {
            maximum as f32
        };
        [white; 4]
    }

    fn cam_to_working(xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
        let cam_to_xyz = normalized_pseudoinverse(xyz_to_cam);

        let xyz_to_rec2020 = [
            [ 1.7166512, -0.3556708, -0.2533663],
            [-0.6666844,  1.6164812,  0.0157685],
            [ 0.0176399, -0.0428107,  0.9425388],
        ];

        let mut out = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                out[row][col] = xyz_to_rec2020[row][0] * cam_to_xyz[0][col]
                    + xyz_to_rec2020[row][1] * cam_to_xyz[1][col]
                    + xyz_to_rec2020[row][2] * cam_to_xyz[2][col];
            }
        }

        for row in 0..3 {
            out[row][1] += out[row][3];
        }

        if out.iter().flatten().any(|v| !v.is_finite())
            || out.iter().flatten().all(|v| *v == 0.0)
        {
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ]
        } else {
            out
        }
    }

    fn camera_to_working_matrix(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
    ) -> [[f32; 4]; 3] {
        if let Some(xyz_to_cam) = interpolated_dng_xyz_to_cam(color, wb_coeffs) {
            cam_to_working(xyz_to_cam)
        } else {
            cam_to_working(color.cam_xyz)
        }
    }

    fn interpolated_dng_xyz_to_cam(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
    ) -> Option<[[f32; 3]; 4]> {
        let matrix0 = calibrated_dng_xyz_to_cam(&color.dng_color[0])?;
        let matrix1 = calibrated_dng_xyz_to_cam(&color.dng_color[1])?;
        let cct0 = calibration_illuminant_cct(color.dng_color[0].illuminant)?;
        let cct1 = calibration_illuminant_cct(color.dng_color[1].illuminant)?;
        let scene_cct = estimate_scene_cct(color, wb_coeffs)?;

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

    fn estimate_scene_cct(color: &ffi::libraw_colordata_t, wb_coeffs: [f32; 4]) -> Option<f32> {
        let mut best_cct = 0.0;
        let mut best_error = f32::INFINITY;

        for row in color.WBCT_Coeffs {
            let cct = row[0];
            if !cct.is_finite() || cct <= 0.0 {
                continue;
            }

            let candidate = white_balance([row[1], row[2], row[3], row[4]]);
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
            let sum = row.iter().sum::<f32>();
            if sum != 0.0 {
                for value in row {
                    *value /= sum;
                }
            }
        }

        pseudoinverse(xyz_to_cam)
    }

    fn pseudoinverse(input: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
        let mut temp = [[0.0; 6]; 3];

        for i in 0..3 {
            for j in 0..6 {
                temp[i][j] = if j == i + 3 { 1.0 } else { 0.0 };
            }
            for j in 0..3 {
                for row in &input {
                    temp[i][j] += row[i] * row[j];
                }
            }
        }

        for i in 0..3 {
            let pivot = temp[i][i];
            if pivot.abs() < 1e-12 {
                return [[0.0; 4]; 3];
            }
            for j in 0..6 {
                temp[i][j] /= pivot;
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
                for k in 0..3 {
                    out[row][col] += temp[row][k + 3] * input[col][k];
                }
            }
        }
        out
    }

    fn c_array_to_string(value: &[c_char]) -> String {
        let ptr = value.as_ptr();
        if ptr.is_null() {
            return String::new();
        }

        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .trim()
            .to_owned()
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
}