#[allow(unused_imports)]
use super::color_profile::CameraProfile;
#[allow(unused_imports)]
use anyhow::{anyhow, Context, Result};
use std::path::Path;

pub const MAX_RAW_EDGE: u32 = 32_768;
#[cfg(target_os = "android")]
pub const MAX_RAW_PIXELS: u64 = 50_000_000;
#[cfg(not(target_os = "android"))]
pub const MAX_RAW_PIXELS: u64 = 120_000_000;
#[cfg(all(libraw_available, target_os = "android"))]
const MAX_RAW_FILE_BYTES: u64 = 2_000_000_000;
#[cfg(all(libraw_available, not(target_os = "android")))]
const MAX_RAW_FILE_BYTES: u64 = 8_000_000_000;
#[cfg(all(libraw_available, target_os = "android"))]
const MAX_SENSOR_PIXELS: u64 = 70_000_000;
#[cfg(all(libraw_available, not(target_os = "android")))]
const MAX_SENSOR_PIXELS: u64 = 160_000_000;
#[cfg(libraw_available)]
const MAX_SENSOR_EDGE: u32 = 40_000;

pub fn validate_raw_dimensions(width: u32, height: u32) -> Result<usize> {
    anyhow::ensure!(width > 0 && height > 0, "RAW dimensions must be non-zero");
    anyhow::ensure!(
        width <= MAX_RAW_EDGE && height <= MAX_RAW_EDGE,
        "RAW dimensions {width}x{height} exceed the {MAX_RAW_EDGE}-pixel edge limit"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("RAW pixel count overflow")?;
    anyhow::ensure!(
        pixels <= MAX_RAW_PIXELS,
        "RAW dimensions {width}x{height} contain {pixels} pixels; the limit is {MAX_RAW_PIXELS}"
    );
    usize::try_from(pixels).context("RAW pixel count does not fit this platform")
}

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
    /// DCP creative profile stages and retained embedded camera ICC data.
    pub camera_profile: CameraProfile,
}

#[cfg(not(libraw_available))]
pub fn load_raw_file(_path: &Path) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_file_with_dcp(_path: &Path, _profile_path: &Path) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(libraw_available)]
pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file(path)
}

#[cfg(libraw_available)]
pub fn load_raw_file_with_dcp(path: &Path, profile_path: &Path) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file_with_dcp(path, profile_path)
}

#[cfg(libraw_available)]
mod libraw_loader {
    use super::{
        validate_raw_dimensions, CameraProfile, CfaKind, LoadedRaw, MAX_RAW_FILE_BYTES,
        MAX_SENSOR_EDGE, MAX_SENSOR_PIXELS,
    };
    use crate::pipeline::color_profile::{DcpMatrixSet, DcpProfile};
    use anyhow::{anyhow, Context, Result};
    use std::ffi::{CStr, CString};
    use std::fs;
    use std::os::raw::c_char;
    use std::path::Path;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    const MAX_DCP_FILE_BYTES: u64 = 64 * 1024 * 1024;

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
        validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;
        load_raw_file_with_selected_profile(path, read_optional_profile(path))
    }

    pub fn load_raw_file_with_dcp(path: &Path, profile_path: &Path) -> Result<LoadedRaw> {
        validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;
        validate_input_file(profile_path, MAX_DCP_FILE_BYTES, "DCP profile")?;
        let mut selected = DcpProfile::from_path(profile_path)
            .with_context(|| format!("read DCP profile {}", profile_path.display()))?
            .ok_or_else(|| anyhow!("{} is not a DNG camera profile", profile_path.display()))?;

        // CameraCalibration belongs to the raw DNG, while the compatibility
        // signature belongs to the selected profile. Carry the camera-side
        // signature into an external profile before evaluating the matrix path.
        if let Some(raw_profile) = read_optional_profile(path) {
            selected.camera_calibration_signature = raw_profile.camera_calibration_signature;
        }
        load_raw_file_with_selected_profile(path, Some(selected))
    }

    fn validate_input_file(path: &Path, maximum_bytes: u64, label: &str) -> Result<()> {
        let source =
            fs::metadata(path).with_context(|| format!("inspect {label} {}", path.display()))?;
        anyhow::ensure!(source.is_file(), "{label} is not a regular file");
        anyhow::ensure!(source.len() > 0, "{label} is empty");
        anyhow::ensure!(
            source.len() <= maximum_bytes,
            "{label} is {} bytes; the safe input limit is {maximum_bytes}",
            source.len()
        );
        Ok(())
    }

    fn read_optional_profile(path: &Path) -> Option<DcpProfile> {
        // DCP tags can be embedded directly in a DNG. Treat malformed optional
        // creative-profile metadata as non-fatal while preserving a diagnostic.
        match DcpProfile::from_path(path) {
            Ok(profile) => profile,
            Err(error) => {
                log::warn!(
                    "ignoring malformed embedded DCP profile in {}: {error:#}",
                    path.display()
                );
                None
            }
        }
    }

    fn load_raw_file_with_selected_profile(
        path: &Path,
        dcp_profile: Option<DcpProfile>,
    ) -> Result<LoadedRaw> {
        validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;

        let c_path = path_to_libraw_cstring(path)?;
        let ctx = LibRawContext::new()?;

        check_libraw(
            unsafe { ffi::libraw_open_file(ctx.raw, c_path.as_ptr()) },
            "open RAW file",
        )?;
        // LibRaw exposes dimensions after open_file. Reject hostile geometry
        // before unpack can allocate the full decoded sensor buffer.
        unsafe { validate_opened_raw_geometry(&ctx) }?;
        check_libraw(unsafe { ffi::libraw_unpack(ctx.raw) }, "unpack RAW file")?;

        unsafe { loaded_raw_from_context(&ctx, dcp_profile) }
    }

    #[cfg(unix)]
    fn path_to_libraw_cstring(path: &Path) -> Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("RAW path contains an interior NUL byte: {}", path.display()))
    }

    #[cfg(not(unix))]
    fn path_to_libraw_cstring(path: &Path) -> Result<CString> {
        let utf8 = path.to_str().with_context(|| {
            format!(
                "LibRaw requires a Unicode path on this platform: {}",
                path.display()
            )
        })?;
        CString::new(utf8.as_bytes())
            .with_context(|| format!("RAW path contains an interior NUL byte: {}", path.display()))
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

    unsafe fn validate_opened_raw_geometry(ctx: &LibRawContext) -> Result<()> {
        let raw = &*ctx.raw;
        let sizes = &raw.rawdata.sizes;
        let active_width = sizes.width as u32;
        let active_height = sizes.height as u32;
        validate_raw_dimensions(active_width, active_height)
            .context("LibRaw header reports an image too large to unpack safely")?;

        let sensor_width = sizes.raw_width as u32;
        let sensor_height = sizes.raw_height as u32;
        anyhow::ensure!(
            sensor_width > 0 && sensor_height > 0,
            "LibRaw header reports empty sensor dimensions"
        );
        anyhow::ensure!(
            sensor_width <= MAX_SENSOR_EDGE && sensor_height <= MAX_SENSOR_EDGE,
            "LibRaw sensor dimensions {sensor_width}x{sensor_height} exceed the {MAX_SENSOR_EDGE}-pixel edge limit"
        );
        let sensor_pixels = u64::from(sensor_width)
            .checked_mul(u64::from(sensor_height))
            .context("LibRaw sensor pixel count overflow")?;
        anyhow::ensure!(
            sensor_pixels <= MAX_SENSOR_PIXELS,
            "LibRaw sensor dimensions {sensor_width}x{sensor_height} contain {sensor_pixels} pixels; the safe unpack limit is {MAX_SENSOR_PIXELS}"
        );
        let minimum_pitch = u64::from(sensor_width)
            .checked_mul(std::mem::size_of::<u16>() as u64)
            .context("LibRaw sensor pitch overflow")?;
        let raw_pitch = u64::from(sizes.raw_pitch);
        // Some LibRaw decoders leave raw_pitch at zero until unpack. The
        // sensor pixel cap still bounds that allocation; validate a declared
        // pitch only when the header actually supplies one.
        anyhow::ensure!(
            raw_pitch == 0 || (raw_pitch >= minimum_pitch && raw_pitch <= 1_073_741_824),
            "LibRaw header reports invalid raw pitch {raw_pitch} for width {sensor_width}"
        );
        Ok(())
    }

    unsafe fn loaded_raw_from_context(
        ctx: &LibRawContext,
        dcp_profile: Option<DcpProfile>,
    ) -> Result<LoadedRaw> {
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
        validate_raw_dimensions(width, height)
            .context("LibRaw reported an image too large to process safely")?;
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
        let (width, height, raw_pixels, color_indices, black_levels_per_pixel) =
            copy_active_pixels(
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
        let calibration_compatible = dcp_profile
            .as_ref()
            .map_or(true, DcpProfile::calibration_is_compatible);
        let (cam_to_srgb, profile_weight) = camera_to_working_matrix(
            color,
            physical_wb,
            cdesc,
            dcp_profile.as_ref(),
            calibration_compatible,
        )?;
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

        let mut camera_profile = dcp_profile
            .map(|profile| CameraProfile::from_dcp(profile, profile_weight))
            .unwrap_or_default();
        let baseline_exposure = color.dng_levels.baseline_exposure as f32;
        // LibRaw initializes a missing BaselineExposure to a sentinel below
        // -999 EV. It is finite, but applying it makes exp2(EV) underflow to
        // zero and turns every non-DNG/proprietary RAW preview black.
        if baseline_exposure.is_finite() && baseline_exposure > -999.0 {
            camera_profile.baseline_exposure_offset += baseline_exposure;
        }
        if !color.profile.is_null() && color.profile_length > 0 {
            let length = usize::try_from(color.profile_length).unwrap_or(0);
            if length <= 16 * 1024 * 1024 {
                let source = std::slice::from_raw_parts(color.profile as *const u8, length);
                let mut profile = Vec::new();
                profile
                    .try_reserve_exact(length)
                    .context("reserve embedded camera ICC profile")?;
                profile.extend_from_slice(source);
                camera_profile.embedded_camera_icc = Some(profile);
            } else {
                log::warn!("ignoring embedded camera ICC profile larger than 16 MiB");
            }
        }

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
            camera_profile,
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
        validate_raw_dimensions(out_width as u32, out_height as u32)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(output_len)
            .context("reserve oriented RAW pixel buffer")?;
        pixels.resize(output_len, 0);
        let mut colors = Vec::new();
        colors
            .try_reserve_exact(output_len)
            .context("reserve oriented CFA buffer")?;
        let mut black_map = Vec::new();
        black_map
            .try_reserve_exact(output_len)
            .context("reserve oriented black-level buffer")?;

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

    fn white_levels(maximum: u32, linear_max: [u32; 4], black_levels: [f32; 4]) -> [f32; 4] {
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

    #[derive(Clone, Copy)]
    struct InterpolatedDngProfile {
        color_matrix: [[f32; 3]; 4],
        calibration: [[f32; 4]; 4],
        forward_matrix: Option<[[f32; 4]; 3]>,
        weight: f32,
    }

    fn camera_to_working_matrix(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
        parsed_profile: Option<&DcpProfile>,
        calibration_compatible: bool,
    ) -> Result<([[f32; 4]; 3], f32)> {
        let analog_balance = analog_balance_matrix(color.dng_levels.analogbalance);
        // Prefer the profile records parsed directly from the selected DNG/DCP
        // IFD. LibRaw remains the fallback for proprietary RAW files and DNGs
        // whose optional profile IFD could not be read.
        let dng_profile = parsed_profile
            .and_then(|profile| {
                interpolated_parsed_dng_profile(
                    profile,
                    color,
                    wb_coeffs,
                    cdesc,
                    analog_balance,
                    calibration_compatible,
                )
            })
            .or_else(|| {
                interpolated_dng_profile(
                    color,
                    wb_coeffs,
                    cdesc,
                    analog_balance,
                    calibration_compatible,
                )
            });
        let (matrix, weight) = if let Some(profile) = dng_profile {
            (
                dng_camera_to_working(profile, analog_balance, wb_coeffs, cdesc)?,
                profile.weight,
            )
        } else {
            // Proprietary RAW formats generally expose LibRaw's consolidated
            // XYZ->camera matrix rather than individual DNG tags.
            (cam_to_working(color.cam_xyz, cdesc), 0.0)
        };

        if matrix.iter().flatten().any(|value| !value.is_finite())
            || matrix.iter().flatten().all(|value| value.abs() <= 1e-12)
        {
            return Err(anyhow!(
                "LibRaw did not provide an invertible camera colour matrix; refusing to treat camera RGB as the working colour space"
            ));
        }
        Ok((matrix, weight))
    }

    fn interpolated_parsed_dng_profile(
        profile: &DcpProfile,
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
        analog_balance: [[f32; 4]; 4],
        calibration_compatible: bool,
    ) -> Option<InterpolatedDngProfile> {
        let first = &profile.matrices[0];
        let second = &profile.matrices[1];
        let valid = [
            first.color_matrix.is_some_and(matrix4x3_is_valid),
            second.color_matrix.is_some_and(matrix4x3_is_valid),
        ];
        match valid {
            [false, false] => return None,
            [true, false] => {
                return parsed_single_dng_profile(
                    first,
                    color.dng_color[0].calibration,
                    0.0,
                    calibration_compatible,
                )
            }
            [false, true] => {
                return parsed_single_dng_profile(
                    second,
                    color.dng_color[1].calibration,
                    1.0,
                    calibration_compatible,
                )
            }
            [true, true] => {}
        }

        let cct0 = calibration_illuminant_cct(first.illuminant?)?;
        let cct1 = calibration_illuminant_cct(second.illuminant?)?;
        let mut scene_cct =
            estimate_scene_cct(color, wb_coeffs, cdesc).unwrap_or_else(|| (cct0 * cct1).sqrt());
        let neutral = camera_neutral(wb_coeffs);
        let first_color = first.color_matrix?;
        let second_color = second.color_matrix?;
        let first_calibration = parsed_calibration(
            first,
            color.dng_color[0].calibration,
            calibration_compatible,
        );
        let second_calibration = parsed_calibration(
            second,
            color.dng_color[1].calibration,
            calibration_compatible,
        );

        let mut weight = mired_interpolation_weight(scene_cct, cct0, cct1);
        for _ in 0..6 {
            let color_matrix = lerp_4x3(first_color, second_color, weight);
            let calibration = lerp_4x4(first_calibration, second_calibration, weight);
            let abcc = multiply_4x4(analog_balance, calibration);
            let xyz_to_camera = multiply_4x4_4x3(abcc, color_matrix);
            let camera_to_xyz = pseudoinverse(xyz_to_camera);
            let white_xyz = multiply_3x4_vector(camera_to_xyz, neutral);
            if let Some(refined) = xyz_to_cct(white_xyz) {
                scene_cct = refined.clamp(1500.0, 50_000.0);
                weight = mired_interpolation_weight(scene_cct, cct0, cct1);
            }
        }

        Some(InterpolatedDngProfile {
            color_matrix: lerp_4x3(first_color, second_color, weight),
            calibration: lerp_4x4(first_calibration, second_calibration, weight),
            forward_matrix: interpolate_optional_forward_matrix(
                first.forward_matrix,
                second.forward_matrix,
                weight,
            ),
            weight,
        })
    }

    fn parsed_single_dng_profile(
        set: &DcpMatrixSet,
        fallback_calibration: [[f32; 4]; 4],
        weight: f32,
        calibration_compatible: bool,
    ) -> Option<InterpolatedDngProfile> {
        Some(InterpolatedDngProfile {
            color_matrix: set.color_matrix?,
            calibration: parsed_calibration(set, fallback_calibration, calibration_compatible),
            forward_matrix: set
                .forward_matrix
                .filter(|matrix| matrix3x4_is_valid(*matrix)),
            weight,
        })
    }

    fn parsed_calibration(
        set: &DcpMatrixSet,
        fallback: [[f32; 4]; 4],
        calibration_compatible: bool,
    ) -> [[f32; 4]; 4] {
        if calibration_compatible {
            set.camera_calibration
                .filter(|matrix| matrix4x4_is_valid(*matrix))
                .unwrap_or_else(|| identity_fallback_4x4(fallback))
        } else {
            identity_4x4()
        }
    }

    fn interpolated_dng_profile(
        color: &ffi::libraw_colordata_t,
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
        analog_balance: [[f32; 4]; 4],
        calibration_compatible: bool,
    ) -> Option<InterpolatedDngProfile> {
        let valid = [
            matrix4x3_is_valid(color.dng_color[0].colormatrix),
            matrix4x3_is_valid(color.dng_color[1].colormatrix),
        ];
        match valid {
            [false, false] => return None,
            [true, false] => {
                return Some(single_dng_profile(
                    &color.dng_color[0],
                    0.0,
                    calibration_compatible,
                ));
            }
            [false, true] => {
                return Some(single_dng_profile(
                    &color.dng_color[1],
                    1.0,
                    calibration_compatible,
                ));
            }
            [true, true] => {}
        }

        let cct0 = calibration_illuminant_cct(color.dng_color[0].illuminant)?;
        let cct1 = calibration_illuminant_cct(color.dng_color[1].illuminant)?;
        let mut scene_cct =
            estimate_scene_cct(color, wb_coeffs, cdesc).unwrap_or_else(|| (cct0 * cct1).sqrt());
        let neutral = camera_neutral(wb_coeffs);

        // DNG interpolation is linear in reciprocal correlated colour
        // temperature. Refine the initial metadata estimate from the actual
        // AsShotNeutral response so files without a WBCT table still select the
        // correct profile blend.
        let mut weight = mired_interpolation_weight(scene_cct, cct0, cct1);
        for _ in 0..6 {
            let color_matrix = lerp_4x3(
                color.dng_color[0].colormatrix,
                color.dng_color[1].colormatrix,
                weight,
            );
            let calibration = if calibration_compatible {
                lerp_4x4(
                    identity_fallback_4x4(color.dng_color[0].calibration),
                    identity_fallback_4x4(color.dng_color[1].calibration),
                    weight,
                )
            } else {
                identity_4x4()
            };
            let abcc = multiply_4x4(analog_balance, calibration);
            let xyz_to_camera = multiply_4x4_4x3(abcc, color_matrix);
            let camera_to_xyz = pseudoinverse(xyz_to_camera);
            let white_xyz = multiply_3x4_vector(camera_to_xyz, neutral);
            if let Some(refined) = xyz_to_cct(white_xyz) {
                scene_cct = refined.clamp(1500.0, 50_000.0);
                weight = mired_interpolation_weight(scene_cct, cct0, cct1);
            }
        }

        let color_matrix = lerp_4x3(
            color.dng_color[0].colormatrix,
            color.dng_color[1].colormatrix,
            weight,
        );
        let calibration = if calibration_compatible {
            lerp_4x4(
                identity_fallback_4x4(color.dng_color[0].calibration),
                identity_fallback_4x4(color.dng_color[1].calibration),
                weight,
            )
        } else {
            identity_4x4()
        };
        let forward_matrix = interpolate_forward_matrix(
            color.dng_color[0].forwardmatrix,
            color.dng_color[1].forwardmatrix,
            weight,
        );
        Some(InterpolatedDngProfile {
            color_matrix,
            calibration,
            forward_matrix,
            weight,
        })
    }

    fn single_dng_profile(
        dng: &ffi::libraw_dng_color_t,
        weight: f32,
        calibration_compatible: bool,
    ) -> InterpolatedDngProfile {
        InterpolatedDngProfile {
            color_matrix: dng.colormatrix,
            calibration: if calibration_compatible {
                identity_fallback_4x4(dng.calibration)
            } else {
                identity_4x4()
            },
            forward_matrix: matrix3x4_is_valid(dng.forwardmatrix).then_some(dng.forwardmatrix),
            weight,
        }
    }

    fn dng_camera_to_working(
        profile: InterpolatedDngProfile,
        analog_balance: [[f32; 4]; 4],
        wb_coeffs: [f32; 4],
        cdesc: [u8; 4],
    ) -> Result<[[f32; 4]; 3]> {
        let abcc = multiply_4x4(analog_balance, profile.calibration);
        let neutral = camera_neutral(wb_coeffs);

        let camera_to_xyz_d50 = if let Some(forward) = profile.forward_matrix {
            // DNG 1.7: FM * D * inverse(AB * CC), where D white-balances
            // reference-camera coordinates using ReferenceNeutral.
            let inverse_abcc = invert_4x4(abcc)
                .ok_or_else(|| anyhow!("DNG AnalogBalance * CameraCalibration is singular"))?;
            let reference_neutral = multiply_4x4_vector(inverse_abcc, neutral);
            let mut balanced_reference_to_xyz = forward;
            for column in 0..4 {
                let value = reference_neutral[column];
                if !value.is_finite() || value.abs() < 1e-10 {
                    return Err(anyhow!("DNG ReferenceNeutral contains an invalid channel"));
                }
                for row in &mut balanced_reference_to_xyz {
                    row[column] /= value;
                }
            }
            multiply_3x4_4x4(balanced_reference_to_xyz, inverse_abcc)
        } else {
            // Without ForwardMatrix, invert AB*CC*CM and chromatically adapt
            // the scene white represented by CameraNeutral to PCS D50.
            let xyz_to_camera = multiply_4x4_4x3(abcc, profile.color_matrix);
            let camera_to_xyz = pseudoinverse(xyz_to_camera);
            if camera_to_xyz
                .iter()
                .flatten()
                .all(|value| value.abs() <= 1e-12)
            {
                return Err(anyhow!("DNG XYZ-to-camera matrix is singular"));
            }
            let source_white = multiply_3x4_vector(camera_to_xyz, neutral);
            let adaptation = bradford_adaptation(source_white, [0.964_22, 1.0, 0.825_21])
                .ok_or_else(|| anyhow!("DNG CameraNeutral does not define a valid white point"))?;
            multiply_3x3_3x4(adaptation, camera_to_xyz)
        };

        // DNG's PCS is D50 while the scene working space is linear Rec.2020
        // D65. Adapt once, then factor out the white balance already applied to
        // CFA samples on the GPU.
        const D50_TO_D65: [[f32; 3]; 3] = [
            [0.955_473_4, -0.023_098_5, 0.063_259_3],
            [-0.028_369_7, 1.009_995_5, 0.021_041_4],
            [0.012_314, -0.020_507_7, 1.330_365_9],
        ];
        let xyz_d50_to_rec2020 = multiply_3x3(XYZ_TO_REC2020, D50_TO_D65);
        let mut physical = multiply_3x3_3x4(xyz_d50_to_rec2020, camera_to_xyz_d50);
        for column in 0..4 {
            let gain = wb_coeffs[column].max(1e-8);
            for row in &mut physical {
                row[column] /= gain;
            }
        }
        Ok(fold_physical_camera_planes(physical, cdesc))
    }

    fn fold_physical_camera_planes(physical: [[f32; 4]; 3], cdesc: [u8; 4]) -> [[f32; 4]; 3] {
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

    fn analog_balance_matrix(values: [f32; 4]) -> [[f32; 4]; 4] {
        let mut out = [[0.0; 4]; 4];
        for index in 0..4 {
            out[index][index] = if values[index].is_finite() && values[index] > 1e-8 {
                values[index]
            } else {
                1.0
            };
        }
        out
    }

    fn camera_neutral(wb_coeffs: [f32; 4]) -> [f32; 4] {
        wb_coeffs.map(|gain| 1.0 / gain.max(1e-8))
    }

    fn interpolate_forward_matrix(
        first: [[f32; 4]; 3],
        second: [[f32; 4]; 3],
        weight: f32,
    ) -> Option<[[f32; 4]; 3]> {
        match (matrix3x4_is_valid(first), matrix3x4_is_valid(second)) {
            (true, true) => Some(lerp_3x4(first, second, weight)),
            (true, false) => Some(first),
            (false, true) => Some(second),
            (false, false) => None,
        }
    }

    fn interpolate_optional_forward_matrix(
        first: Option<[[f32; 4]; 3]>,
        second: Option<[[f32; 4]; 3]>,
        weight: f32,
    ) -> Option<[[f32; 4]; 3]> {
        match (
            first.filter(|matrix| matrix3x4_is_valid(*matrix)),
            second.filter(|matrix| matrix3x4_is_valid(*matrix)),
        ) {
            (Some(a), Some(b)) => Some(lerp_3x4(a, b, weight)),
            (Some(matrix), None) | (None, Some(matrix)) => Some(matrix),
            (None, None) => None,
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
            identity_4x4()
        }
    }

    fn identity_4x4() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn matrix4x3_is_valid(matrix: [[f32; 3]; 4]) -> bool {
        matrix.iter().flatten().all(|v| v.is_finite())
            && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
    }

    fn matrix3x4_is_valid(matrix: [[f32; 4]; 3]) -> bool {
        matrix.iter().flatten().all(|v| v.is_finite())
            && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
    }

    fn matrix4x4_is_valid(matrix: [[f32; 4]; 4]) -> bool {
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
            1 => Some(5500.0),  // Daylight
            2 => Some(4000.0),  // Fluorescent
            3 => Some(2856.0),  // Tungsten
            4 => Some(5500.0),  // Flash
            9 => Some(5500.0),  // Fine weather
            10 => Some(6500.0), // Cloudy weather
            11 => Some(7500.0), // Shade
            12 => Some(6500.0), // Daylight fluorescent
            13 => Some(5000.0), // Day white fluorescent
            14 => Some(4150.0), // Cool white fluorescent
            15 => Some(3500.0), // White fluorescent
            16 => Some(3000.0), // Warm white fluorescent
            17 => Some(2856.0), // Standard light A
            18 => Some(4874.0), // Standard light B
            19 => Some(6774.0), // Standard light C
            20 => Some(5503.0), // D55
            21 => Some(6504.0), // D65
            22 => Some(7504.0), // D75
            23 => Some(5003.0), // D50
            24 => Some(3200.0), // ISO studio tungsten
            _ => None,
        }
    }

    fn mired_interpolation_weight(cct: f32, first_cct: f32, second_cct: f32) -> f32 {
        let first = 1_000_000.0 / first_cct.max(1.0);
        let second = 1_000_000.0 / second_cct.max(1.0);
        let scene = 1_000_000.0 / cct.max(1.0);
        let denominator = second - first;
        if denominator.abs() < 1e-8 {
            0.0
        } else {
            ((scene - first) / denominator).clamp(0.0, 1.0)
        }
    }

    fn xyz_to_cct(xyz: [f32; 3]) -> Option<f32> {
        let sum = xyz[0] + xyz[1] + xyz[2];
        if !sum.is_finite() || sum.abs() < 1e-10 {
            return None;
        }
        let x = xyz[0] / sum;
        let y = xyz[1] / sum;
        let denominator = y - 0.1858;
        if denominator.abs() < 1e-8 {
            return None;
        }
        let n = (x - 0.3320) / denominator;
        let cct = -449.0 * n * n * n + 3525.0 * n * n - 6823.3 * n + 5520.33;
        (cct.is_finite() && cct > 0.0).then_some(cct)
    }

    fn bradford_adaptation(source: [f32; 3], target: [f32; 3]) -> Option<[[f32; 3]; 3]> {
        const BRADFORD: [[f32; 3]; 3] = [
            [0.8951, 0.2664, -0.1614],
            [-0.7502, 1.7135, 0.0367],
            [0.0389, -0.0685, 1.0296],
        ];
        const BRADFORD_INV: [[f32; 3]; 3] = [
            [0.986_992_9, -0.147_054_3, 0.159_962_7],
            [0.432_305_3, 0.518_360_3, 0.049_291_2],
            [-0.008_528_7, 0.040_042_8, 0.968_486_7],
        ];
        if !source.iter().all(|v| v.is_finite()) || source[1].abs() < 1e-10 {
            return None;
        }
        let normalized_source = source.map(|v| v / source[1]);
        let source_lms = multiply_3x3_vector(BRADFORD, normalized_source);
        let target_lms = multiply_3x3_vector(BRADFORD, target);
        if source_lms.iter().any(|v| !v.is_finite() || v.abs() < 1e-10) {
            return None;
        }
        let diagonal = [
            [target_lms[0] / source_lms[0], 0.0, 0.0],
            [0.0, target_lms[1] / source_lms[1], 0.0],
            [0.0, 0.0, target_lms[2] / source_lms[2]],
        ];
        Some(multiply_3x3(BRADFORD_INV, multiply_3x3(diagonal, BRADFORD)))
    }

    fn lerp_4x3(a: [[f32; 3]; 4], b: [[f32; 3]; 4], t: f32) -> [[f32; 3]; 4] {
        let mut out = [[0.0; 3]; 4];
        for row in 0..4 {
            for col in 0..3 {
                out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
            }
        }
        out
    }

    fn lerp_3x4(a: [[f32; 4]; 3], b: [[f32; 4]; 3], t: f32) -> [[f32; 4]; 3] {
        let mut out = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
            }
        }
        out
    }

    fn lerp_4x4(a: [[f32; 4]; 4], b: [[f32; 4]; 4], t: f32) -> [[f32; 4]; 4] {
        let mut out = [[0.0; 4]; 4];
        for row in 0..4 {
            for col in 0..4 {
                out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
            }
        }
        out
    }

    fn multiply_4x4(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
        let mut out = [[0.0; 4]; 4];
        for row in 0..4 {
            for col in 0..4 {
                for k in 0..4 {
                    out[row][col] += a[row][k] * b[k][col];
                }
            }
        }
        out
    }

    fn multiply_4x4_4x3(a: [[f32; 4]; 4], b: [[f32; 3]; 4]) -> [[f32; 3]; 4] {
        let mut out = [[0.0; 3]; 4];
        for row in 0..4 {
            for col in 0..3 {
                for k in 0..4 {
                    out[row][col] += a[row][k] * b[k][col];
                }
            }
        }
        out
    }

    fn multiply_3x4_4x4(a: [[f32; 4]; 3], b: [[f32; 4]; 4]) -> [[f32; 4]; 3] {
        let mut out = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                for k in 0..4 {
                    out[row][col] += a[row][k] * b[k][col];
                }
            }
        }
        out
    }

    fn multiply_3x3(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
        let mut out = [[0.0; 3]; 3];
        for row in 0..3 {
            for col in 0..3 {
                for k in 0..3 {
                    out[row][col] += a[row][k] * b[k][col];
                }
            }
        }
        out
    }

    fn multiply_3x3_3x4(a: [[f32; 3]; 3], b: [[f32; 4]; 3]) -> [[f32; 4]; 3] {
        let mut out = [[0.0; 4]; 3];
        for row in 0..3 {
            for col in 0..4 {
                for k in 0..3 {
                    out[row][col] += a[row][k] * b[k][col];
                }
            }
        }
        out
    }

    fn multiply_4x4_vector(matrix: [[f32; 4]; 4], vector: [f32; 4]) -> [f32; 4] {
        matrix.map(|row| {
            row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2] + row[3] * vector[3]
        })
    }

    fn multiply_3x4_vector(matrix: [[f32; 4]; 3], vector: [f32; 4]) -> [f32; 3] {
        matrix.map(|row| {
            row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2] + row[3] * vector[3]
        })
    }

    fn multiply_3x3_vector(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
        matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    }

    fn invert_4x4(matrix: [[f32; 4]; 4]) -> Option<[[f32; 4]; 4]> {
        let mut augmented = [[0.0f64; 8]; 4];
        for row in 0..4 {
            for col in 0..4 {
                augmented[row][col] = f64::from(matrix[row][col]);
            }
            augmented[row][row + 4] = 1.0;
        }
        for pivot in 0..4 {
            let mut best = pivot;
            for row in pivot + 1..4 {
                if augmented[row][pivot].abs() > augmented[best][pivot].abs() {
                    best = row;
                }
            }
            if !augmented[best][pivot].is_finite() || augmented[best][pivot].abs() < 1e-14 {
                return None;
            }
            augmented.swap(pivot, best);
            let divisor = augmented[pivot][pivot];
            for value in &mut augmented[pivot] {
                *value /= divisor;
            }
            for row in 0..4 {
                if row == pivot {
                    continue;
                }
                let factor = augmented[row][pivot];
                for col in 0..8 {
                    augmented[row][col] -= factor * augmented[pivot][col];
                }
            }
        }
        let mut out = [[0.0; 4]; 4];
        for row in 0..4 {
            for col in 0..4 {
                let value = augmented[row][col + 4];
                if !value.is_finite() {
                    return None;
                }
                out[row][col] = value as f32;
            }
        }
        Some(out)
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
