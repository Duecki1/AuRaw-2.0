//! Loads a RAW file with `rawler` and prepares it for GPU upload.
//!
//! We deliberately do the *absolute minimum* on the CPU:
//!   - decode (Crawler automatically linearizes compressed formats like Sony ARW during decoding)
//!   - normalize sensor data to a 0.0..1.0 f32 buffer (black/white level only)
//!   - extract white balance coefficients + camera->XYZ matrix
//!
//! Demosaic, white balance multiply, color matrix, and exposure all happen
//! on the GPU in pipeline.wgsl. Nothing here touches pixel color.

use anyhow::{anyhow, Context, Result};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::Dim2;
use rawler::rawimage::{RawImage, RawImageData};
use rawler::rawsource::RawSource;
use std::path::Path;

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

    /// Camera RGB -> sRGB (D65) linear matrix, row-major 3x3, derived from
    /// rawler's camera->XYZ matrix composed with the standard XYZ->sRGB
    /// matrix.
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

/// Map rawler's CFA description to our shader's 0..3 pattern id.
fn cfa_pattern_id(raw: &RawImage) -> u32 {
    let pattern_str = raw.camera.cfa.name.to_uppercase();
    match pattern_str.as_str() {
        "RGGB" => 0,
        "BGGR" => 1,
        "GRBG" => 2,
        "GBRG" => 3,
        _ => 0, // sensible default
    }
}

pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    let source = RawSource::new(path).with_context(|| format!("opening {}", path.display()))?;

    let mut raw_image: RawImage = rawler::decode(&source, &RawDecodeParams { image_index: 0 })
        .map_err(|e| anyhow!("rawler decode failed: {e}"))?;

    // Apply scaling to map black/white levels to 0.0..=1.0 floats.
    // rawler automatically linearizes compressed Sony data during decode.
    raw_image
        .apply_scaling()
        .map_err(|e| anyhow!("rawler scaling failed: {e}"))?;

    let Dim2 { w: width, h: height } = raw_image.dim();
    let width = width as u32;
    let height = height as u32;

    let raw_pixels: Vec<f32> = match &raw_image.data {
        RawImageData::Float(buf) => buf.clone(),
        RawImageData::Integer(buf) => {
            buf.iter().map(|&v| v as f32 / 65535.0).collect()
        }
    };

    if raw_pixels.len() != (width * height) as usize {
        return Err(anyhow!(
            "unexpected raw buffer size: got {}, expected {}",
            raw_pixels.len(),
            width * height
        ));
    }

    // White balance: Normalize so green channel = 1.0
    let mut wb = raw_image.wb_coeffs;
    let g = if wb[1] != 0.0 { wb[1] } else { 1.0 };
    for c in wb.iter_mut() {
        if *c <= 0.0 || !c.is_finite() {
            *c = g;
        }
    }
    let wb_norm = [wb[0] / g, wb[1] / g, wb[2] / g, wb[3] / g];

    // Extract color matrix
    let cam_to_xyz_raw = raw_image.cam_to_xyz();
    let cam_to_xyz = [
        [cam_to_xyz_raw[0][0], cam_to_xyz_raw[0][1], cam_to_xyz_raw[0][2]],
        [cam_to_xyz_raw[1][0], cam_to_xyz_raw[1][1], cam_to_xyz_raw[1][2]],
        [cam_to_xyz_raw[2][0], cam_to_xyz_raw[2][1], cam_to_xyz_raw[2][2]],
    ];

    let sum_abs: f32 = cam_to_xyz.iter().map(|row| row.iter().map(|v| v.abs()).sum::<f32>()).sum();
    
    let cam_to_srgb = if sum_abs < 1e-5 {
        log::warn!("No color matrix found in raw metadata. Falling back to identity.");
        [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]
    } else {
        mat3_mul(XYZ_TO_SRGB, cam_to_xyz)
    };

    let cfa_pattern = cfa_pattern_id(&raw_image);

    // Diagnostic Logs: Output some values to verify they are loading correctly
    log::info!("---------------- DIAGNOSTIC LOGS ----------------");
    log::info!("Camera Make: {}, Model: {}", raw_image.make, raw_image.model);
    log::info!("Image Dimensions: {} x {}", width, height);
    log::info!("CFA Pattern ID: {}", cfa_pattern);
    log::info!("White Balance Coefficients (normalized): {:?}", wb_norm);
    log::info!("Camera-to-sRGB Color Matrix: {:?}", cam_to_srgb);
    
    // Check if the pixel buffer is filled with non-zero values
    let sample_pixels = &raw_pixels[0..std::cmp::min(10, raw_pixels.len())];
    log::info!("Sample Raw Pixels (First 10 values): {:?}", sample_pixels);
    log::info!("--------------------------------------------------");

    Ok(LoadedRaw {
        raw_pixels,
        width,
        height,
        cfa_pattern,
        wb_coeffs: [wb_norm[0], wb_norm[1], wb_norm[2], wb_norm[3]],
        cam_to_srgb,
        camera_make: raw_image.make.clone(),
        camera_model: raw_image.model.clone(),
    })
}