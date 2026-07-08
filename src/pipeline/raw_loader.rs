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
    use std::ptr;
    use std::slice;

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

        let raw_pixels = copy_raw_pixels(
            rawdata.raw_image,
            raw_width,
            raw_height,
            crop_x,
            crop_y,
            width,
            height,
            sizes.raw_pitch as usize,
        )?;
        let color_indices = color_indices(ctx.raw, crop_x, crop_y, width, height)?;
        let wb_coeffs = white_balance(color.cam_mul);
        let cam_to_srgb = cam_to_srgb(color.cam_xyz);
        let black_levels = black_levels(color.black, &color.cblack);
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

    unsafe fn copy_raw_pixels(
        raw_image: *const u16,
        raw_width: u32,
        raw_height: u32,
        crop_x: u32,
        crop_y: u32,
        width: u32,
        height: u32,
        raw_pitch: usize,
    ) -> Result<Vec<u16>> {
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

        let mut out = vec![0; width * height];
        for y in 0..height {
            let raw_y = crop_y + y;
            let row_ptr = (raw_image as *const u8).add(raw_y * pitch) as *const u16;
            let src = slice::from_raw_parts(row_ptr.add(crop_x), width);
            out[y * width..(y + 1) * width].copy_from_slice(src);
        }

        Ok(out)
    }

    unsafe fn color_indices(
        raw: *mut ffi::libraw_data_t,
        crop_x: u32,
        crop_y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let pixels = width
            .checked_mul(height)
            .ok_or_else(|| anyhow!("RAW dimensions overflow"))? as usize;
        let mut out = Vec::with_capacity(pixels);

        for y in 0..height {
            for x in 0..width {
                let color = ffi::libraw_COLOR(raw, (crop_y + y) as i32, (crop_x + x) as i32);
                out.push(color.clamp(0, 2) as u8);
            }
        }

        Ok(out)
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

    fn black_levels(black: u32, cblack: &[u32]) -> [f32; 4] {
        let mut out = [black as f32; 4];
        for (index, value) in out.iter_mut().enumerate() {
            *value += cblack.get(index).copied().unwrap_or(0) as f32;
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

    fn cam_to_srgb(xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
        let cam_to_xyz = normalized_pseudoinverse(xyz_to_cam);
        let xyz_to_srgb = [
            [3.2404542, -1.5371385, -0.4985314],
            [-0.9692660, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
        ];

        let mut out = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                out[row][col] = xyz_to_srgb[row][0] * cam_to_xyz[0][col]
                    + xyz_to_srgb[row][1] * cam_to_xyz[1][col]
                    + xyz_to_srgb[row][2] * cam_to_xyz[2][col];
            }
        }

        if out.iter().flatten().any(|v| !v.is_finite()) || out.iter().flatten().all(|v| *v == 0.0) {
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ]
        } else {
            out
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
