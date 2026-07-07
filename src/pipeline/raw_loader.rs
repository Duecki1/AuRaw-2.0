//! Loads a RAW file via system LibRaw and prepares it for GPU upload.
//!
//! We deliberately do the *absolute minimum* on the CPU:
//!   - decode (LibRaw handles compressed formats — ARW, CR2, CR3, NEF, etc.)
//!   - normalize sensor data to a 0.0..1.0 f32 buffer (black/white level only)
//!   - extract white balance coefficients + camera->XYZ matrix
//!   - decode the CFA pattern from LibRaw's `filters` field
//!
//! Demosaic, white balance multiply, color matrix, and exposure all happen
//! on the GPU in pipeline.wgsl.

use anyhow::{anyhow, Result};
use std::ffi::{CStr, CString};
use std::path::Path;

// Include the auto-generated FFI bindings
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

// Include the auto-generated FFI bindings for the ported darktable/ansel
// highlight-reconstruction module (highlights.c / highlights.h).
include!(concat!(env!("OUT_DIR"), "/highlights_bindings.rs"));

pub struct LoadedRaw {
    /// Single-channel sensor data, normalized to 0.0..=1.0 (black-level
    /// subtracted, white-level divided). Row-major, `width * height` floats.
    pub raw_pixels: Vec<f32>,
    pub width: u32,
    pub height: u32,

    /// CFA pattern encoded for the shader: 0=RGGB 1=BGGR 2=GRBG 3=GBRG
    pub cfa_pattern: u32,

    /// White balance coefficients (R, G, B, G2), normalized so G ≈ 1.0.
    pub wb_coeffs: [f32; 4],

    /// Camera RGB -> sRGB (D65) linear matrix, row-major 3x3.
    pub cam_to_srgb: [[f32; 3]; 3],

    pub camera_make: String,
    pub camera_model: String,
}

/// XYZ (D65) -> linear sRGB, standard IEC 61966-2-1 primaries.
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut s = 0.0;
            for k in 0..3 {
                s += a[i][k] * b[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

/// Closed-form 3x3 matrix inverse via the adjugate (cofactor) method.
///
/// LibRaw's `cam_xyz` is the DNG `ColorMatrixN`, which maps **XYZ -> camera
/// RGB** (confirmed by LibRaw's own maintainers: "cam_xyz[] matrix is the
/// same matrix as DNG 'ColorMatrixN' (XYZ to Camera)"). To get the
/// camera-RGB -> XYZ matrix we actually need for the pipeline, this matrix
/// must be *inverted*, not transposed. (A transpose is only equal to the
/// inverse for an orthogonal matrix, which a camera color matrix is not —
/// using a transpose here previously caused wildly wrong colors, e.g.
/// negative red / blown-out green on neutral input.)
fn mat3_inverse(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];

    let cof_a = e * i - f * h;
    let cof_b = -(d * i - f * g);
    let cof_c = d * h - e * g;
    let cof_d = -(b * i - c * h);
    let cof_e = a * i - c * g;
    let cof_f = -(a * h - b * g);
    let cof_g = b * f - c * e;
    let cof_h = -(a * f - c * d);
    let cof_i = a * e - b * d;

    let det = a * cof_a + b * cof_b + c * cof_c;
    if det.abs() < 1e-12 {
        log::warn!("cam_xyz matrix is singular or near-singular; falling back to identity.");
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let inv_det = 1.0 / det;

    // Adjugate is the transpose of the cofactor matrix; dividing by det
    // gives the inverse.
    [
        [cof_a * inv_det, cof_d * inv_det, cof_g * inv_det],
        [cof_b * inv_det, cof_e * inv_det, cof_h * inv_det],
        [cof_c * inv_det, cof_f * inv_det, cof_i * inv_det],
    ]
}

/// Normalize each row of a camera-RGB -> sRGB matrix so it sums to 1.
///
/// This mirrors dcraw/LibRaw's `cam_xyz_coeff()`: after computing
/// `cam_rgb = xyz_rgb * cam_xyz` (i.e. our `XYZ_TO_SRGB * cam_to_xyz`),
/// dcraw normalizes each row to sum to 1 before using it. Without this step
/// the matrix is still a mathematically valid transform, but it no longer
/// maps a neutral (gray) camera pixel to a neutral output pixel -- it
/// introduces a systematic color cast (in practice, a magenta/green tilt)
/// even though saturated colors still look roughly plausible.
fn normalize_rows(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = m;
    for row in out.iter_mut() {
        let sum: f32 = row.iter().sum();
        if sum.abs() > 1e-8 {
            for v in row.iter_mut() {
                *v /= sum;
            }
        }
    }
    out
}

/// Decode the CFA pattern from LibRaw's packed `filters` field.
fn decode_cfa_pattern(filters: u32) -> u32 {
    let fc = |x: u32, y: u32| -> u32 {
        (filters >> ((((y << 1) & 14) + (x & 1)) << 1)) & 3
    };
    let tl = fc(0, 0);
    let tr = fc(1, 0);
    let bl = fc(0, 1);
    let br = fc(1, 1);
    match (tl, tr, bl, br) {
        (0, 1, 1, 2) => 0, // RGGB
        (2, 1, 1, 0) => 1, // BGGR
        (1, 0, 2, 1) => 2, // GRBG
        (1, 2, 0, 1) => 3, // GBRG
        _ => {
            log::warn!(
                "Unknown CFA layout (tl={tl}, tr={tr}, bl={bl}, br={br}, filters=0x{filters:08x}); \
                 defaulting to RGGB."
            );
            0
        }
    }
}
/// LibRaw's dcraw `FC` macro. Returns 0=R, 1=G, 2=B, 3=G2
fn libraw_fc(filters: u32, x: u32, y: u32) -> usize {
    let x = x as i32;
    let y = y as i32;
    ((filters >> ((((y << 1) & 14) + (x & 1)) << 1)) & 3) as usize
}

pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    log::info!("LibRaw: opening {}", path.display());

    // 1. Initialize LibRaw
    let handle = unsafe { libraw_init(0) };
    if handle.is_null() {
        return Err(anyhow!("libraw_init failed"));
    }

    // 2. Open file
    let c_path = CString::new(path.to_str().ok_or_else(|| anyhow!("Invalid path string"))?)?;
    let ret = unsafe { libraw_open_file(handle, c_path.as_ptr()) };
    if ret != 0 {
        unsafe { libraw_close(handle) };
        return Err(anyhow!("libraw_open_file failed with code {}", ret));
    }

    // 3. Unpack raw data
    let ret = unsafe { libraw_unpack(handle) };
    if ret != 0 {
        unsafe { libraw_close(handle) };
        return Err(anyhow!("libraw_unpack failed with code {}", ret));
    }

    // 4. Access the data struct
    let imgdata = unsafe { &*handle };

    // --- Geometry ----------------------------------------------------------
    let width = imgdata.sizes.width as u32;
    let height = imgdata.sizes.height as u32;
    let raw_width = imgdata.sizes.raw_width as u32;
    let raw_height = imgdata.sizes.raw_height as u32;
    let top_margin = imgdata.sizes.top_margin as u32;
    let left_margin = imgdata.sizes.left_margin as u32;

    if width == 0 || height == 0 {
        unsafe { libraw_close(handle) };
        return Err(anyhow!("LibRaw reported zero-sized image"));
    }

    // --- Raw sensor pointer ------------------------------------------------
    let raw_ptr = imgdata.rawdata.raw_image;
    if raw_ptr.is_null() {
        unsafe { libraw_close(handle) };
        return Err(anyhow!(
            "LibRaw returned null raw_image pointer. File might be linear DNG or unsupported."
        ));
    }

    // --- Black/white levels ------------------------------------------------
    // Base black level and per-channel black levels (cblack[0..3] = R, G1, B, G2)
    let base_black = imgdata.color.black as f32;
    let maximum = imgdata.color.maximum as f32;
    
    let cblack = [
        imgdata.color.cblack[0] as f32,
        imgdata.color.cblack[1] as f32,
        imgdata.color.cblack[2] as f32,
        imgdata.color.cblack[3] as f32,
    ];

    // Per-channel normalization scale
    let norm_scale = [
        if maximum > cblack[0] { 1.0 / (maximum - cblack[0]) } else { 1.0 },
        if maximum > cblack[1] { 1.0 / (maximum - cblack[1]) } else { 1.0 },
        if maximum > cblack[2] { 1.0 / (maximum - cblack[2]) } else { 1.0 },
        if maximum > cblack[3] { 1.0 / (maximum - cblack[3]) } else { 1.0 },
    ];

    // Fallback if cblack is zero
    let norm_scale_fallback = if maximum > base_black { 1.0 / (maximum - base_black) } else { 1.0 };

    // --- Extract visible region & normalize to 0..1 f32 --------------------
    let filters = imgdata.idata.filters;
    let mut raw_pixels: Vec<f32> = (0..height)
        .flat_map(|y| {
            let ry = (y + top_margin) as usize;
            (0..width).map(move |x| {
                let rx = (x + left_margin) as usize;
                let idx = ry * raw_width as usize + rx;
                let v = unsafe { *raw_ptr.add(idx) } as f32;

                // Determine which color channel this pixel is
                let c = libraw_fc(filters, x, y);
                let black = if cblack[c] > 0.0 { cblack[c] } else { base_black };
                let scale = if cblack[c] > 0.0 { norm_scale[c] } else { norm_scale_fallback };
                
                (v - black) * scale
            })
        })
        .collect();

    // --- Highlight reconstruction (ported darktable/ansel highlights.c) ---
    // This MUST run here: on the still-mosaiced (one sample per pixel),
    // still-per-channel-normalized-but-not-yet-demosaiced buffer. Once
    // demosaic/WB/the color matrix mix channels together there's no way to
    // tell "a genuinely bright highlight" apart from "this one raw channel
    // clipped early" -- which is exactly what produced the magenta/pink
    // highlight blobs instead of a clean white/desaturated roll-off.
    //
    // Since raw_pixels is already normalized to 0..1 per channel (black
    // subtracted, white-level divided), we pass processed_maximum = 1.0 for
    // all four channels here -- NOT the raw sensor `maximum`, which would
    // double-apply the white-level scaling this function expects to do
    // itself in un-normalized darktable pipelines.
    //
    // roi_x/roi_y are passed as 0, 0 (NOT left_margin/top_margin), because
    // raw_pixels is already the margin-stripped, buffer-local extraction --
    // the same convention used by libraw_fc(filters, x, y) above, which
    // also takes buffer-local (not sensor-absolute) x/y. highlights.c's
    // internal FC()/FCxtrans() macros derive Bayer/X-Trans phase from
    // (row + roi_y, col + roi_x), and FC's parity-based lookup only agrees
    // with libraw_fc's phase if this offset is 0 -- passing the sensor
    // margins here would silently shift the CFA phase by one whenever a
    // camera's left_margin/top_margin happens to be odd.
    {
        let mut reconstructed = vec![0.0f32; raw_pixels.len()];
        let processed_maximum: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
        // 0.987 matches the safety margin already used internally by the
        // AURAW_HIGHLIGHTS_INPAINT clip levels in highlights.c, so the
        // "soft" clip point here is consistent with what that mode assumes.
        let clip_threshold = 0.987f32;
        let xtrans_dummy = [[0u8; 6]; 6]; // unused for Bayer sensors (filters != 9)

        unsafe {
            auraw_process_highlights(
                raw_pixels.as_ptr(),
                reconstructed.as_mut_ptr(),
                width as i32,
                height as i32,
                0, // roi_x: buffer-local, see note above
                0, // roi_y: buffer-local, see note above
                filters as i32,
                xtrans_dummy.as_ptr(),
                auraw_highlights_mode::AURAW_HIGHLIGHTS_INPAINT as i32,
                clip_threshold,
                processed_maximum.as_ptr(),
            );
        }
        raw_pixels = reconstructed;
    }

    // --- White balance -----------------------------------------------------
    let cam_mul = imgdata.color.cam_mul;
    let g1 = if cam_mul[1] > 0.0 && cam_mul[1].is_finite() { cam_mul[1] } else { 1.0 };
    let g2 = if cam_mul[3] > 0.0 && cam_mul[3].is_finite() { cam_mul[3] } else { g1 };
    let wb_norm = [cam_mul[0] / g1, 1.0, cam_mul[2] / g1, g2 / g1];

    // --- Color matrix: camera RGB -> XYZ (D65) -----------------------------
    // IMPORTANT: LibRaw's `cam_xyz` is the DNG `ColorMatrixN`, which maps
    // **XYZ -> camera RGB** (confirmed directly by LibRaw's own
    // maintainers, not just inferred from field layout). It is not
    // camera-RGB -> XYZ, and it is not simply the transpose of the matrix
    // we want either. To get camera-RGB -> XYZ we must compute the actual
    // matrix **inverse** of the top-left 3x3 block of cam_xyz (rows 0..3 =
    // R, G1, B; the 4th row, G2, only applies to 4-color sensors and isn't
    // part of this 3x3 transform).
    //
    // (The previous version of this code transposed instead of inverting,
    // which is only valid for orthogonal matrices -- a camera color matrix
    // is not orthogonal, so that produced physically-wrong results, e.g.
    // negative red and blown-out green on a neutral gray input.)
    let xyz_to_cam = [
        [
            imgdata.color.cam_xyz[0][0] as f32,
            imgdata.color.cam_xyz[0][1] as f32,
            imgdata.color.cam_xyz[0][2] as f32,
        ],
        [
            imgdata.color.cam_xyz[1][0] as f32,
            imgdata.color.cam_xyz[1][1] as f32,
            imgdata.color.cam_xyz[1][2] as f32,
        ],
        [
            imgdata.color.cam_xyz[2][0] as f32,
            imgdata.color.cam_xyz[2][1] as f32,
            imgdata.color.cam_xyz[2][2] as f32,
        ],
    ];

    let sum_abs: f32 = xyz_to_cam.iter().flatten().map(|v| v.abs()).sum();
    let cam_to_srgb = if sum_abs < 1e-5 {
        log::warn!("No color matrix found in LibRaw metadata. Falling back to identity.");
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    } else {
        let cam_to_xyz = mat3_inverse(xyz_to_cam);
        let unnormalized = mat3_mul(XYZ_TO_SRGB, cam_to_xyz);
        // Row-normalize so a neutral (gray) camera pixel maps to a neutral
        // sRGB pixel -- this matches dcraw/LibRaw's own cam_xyz_coeff()
        // convention and is what eliminates the residual color cast.
        normalize_rows(unnormalized)
    };

    // --- CFA pattern -------------------------------------------------------
    let cfa_pattern = decode_cfa_pattern(imgdata.idata.filters);

    // --- Camera identity ---------------------------------------------------
    let make_str = unsafe { CStr::from_ptr(imgdata.idata.make.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    let model_str = unsafe { CStr::from_ptr(imgdata.idata.model.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();

    // --- Diagnostics -------------------------------------------------------
    log::info!("---------------- DIAGNOSTIC LOGS ----------------");
    log::info!("Camera Make: {}, Model: {}", make_str, model_str);
    log::info!(
        "Visible: {} x {} | Raw: {} x {} | margin top={} left={}",
        width, height, raw_width, raw_height, top_margin, left_margin
    );
    log::info!(
        "CFA Pattern ID: {} (filters=0x{:08x})",
        cfa_pattern,
        imgdata.idata.filters
    );
    log::info!("White Balance (normalized): {:?}", wb_norm);
    log::info!("Cam->sRGB Matrix: {:?}", cam_to_srgb);
    log::info!("Base Black: {}, Maximum: {}", base_black, maximum);
    log::info!("Per-channel Black (cblack): {:?}", cblack);
    let n = raw_pixels.len().min(10);
    log::info!("Sample Pixels (first {n}): {:?}", &raw_pixels[..n]);
    log::info!("--------------------------------------------------");

    // --- Cleanup -----------------------------------------------------------
    unsafe { libraw_close(handle) };

    Ok(LoadedRaw {
        raw_pixels,
        width,
        height,
        cfa_pattern,
        wb_coeffs: wb_norm,
        cam_to_srgb,
        camera_make: make_str,
        camera_model: model_str,
    })
}