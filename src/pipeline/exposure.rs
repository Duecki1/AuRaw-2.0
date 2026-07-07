//! Direct Rust port of darktable/ansel's `src/iop/exposure.c`.
//!
//! Only the manual-mode path is implemented (EXPOSURE_MODE_MANUAL).
//! Deflicker mode is intentionally left out for this first pass — see the
//! `TODO` at the bottom if you want to add it later; the histogram-based
//! `_compute_correction` logic doesn't have a natural GPU-preview
//! equivalent and would need a compute-shader histogram pass first.

use bytemuck::{Pod, Zeroable};

/// Mirrors `dt_iop_exposure_params_t` (manual-mode fields only).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureParams {
    /// `black`: $MIN: -1.0 $MAX: 1.0 $DEFAULT: 0.0 — "black level correction"
    pub black: f32,
    /// `exposure`: $MIN: -18.0 $MAX: 18.0 $DEFAULT: 0.0 — EV
    pub exposure: f32,
}

impl Default for ExposureParams {
    fn default() -> Self {
        Self {
            black: 0.0,
            exposure: 0.0,
        }
    }
}

/// Result of `_process_common_setup()` for manual mode: just `black` and
/// `scale`, exactly as stored in `dt_iop_exposure_data_t`.
#[derive(Debug, Clone, Copy)]
pub struct ExposureScale {
    pub black: f32,
    pub scale: f32,
}

/// `#define exposure2white(x) exp2f(-(x))`
#[inline]
fn exposure2white(x: f32) -> f32 {
    (-x).exp2()
}

/// `#define white2exposure(x) -dt_log2f(fmaxf(1e-20f, x))`
/// Provided for symmetry / for a future "set exposure from a clicked
/// highlight" tool, matching autoset() in the C file.
#[inline]
#[allow(dead_code)]
fn white2exposure(x: f32) -> f32 {
    -(x.max(1e-20)).log2()
}

impl ExposureParams {
    /// Direct port of the manual-mode branch of `_process_common_setup()`:
    ///
    /// ```c
    /// d->black = d->params.black;
    /// float exposure = d->params.exposure;
    /// // (deflicker branch skipped — manual mode only)
    /// const float white = exposure2white(exposure);
    /// d->scale = 1.0 / (white - d->black);
    /// ```
    pub fn compute_scale(&self) -> ExposureScale {
        let black = self.black;
        let exposure = self.exposure;
        let white = exposure2white(exposure);
        let scale = 1.0 / (white - black);
        ExposureScale { black, scale }
    }

    /// CPU reference implementation of `process()`'s per-pixel body, for
    /// unit-testing the shader against ground truth on a handful of pixels:
    ///
    /// ```c
    /// out[k] = (in[k] - black) * scale;
    /// ```
    #[allow(dead_code)]
    pub fn apply_reference(&self, rgb: [f32; 3]) -> [f32; 3] {
        let ExposureScale { black, scale } = self.compute_scale();
        [
            (rgb[0] - black) * scale,
            (rgb[1] - black) * scale,
            (rgb[2] - black) * scale,
        ]
    }
}

/// GPU uniform buffer layout — must match `struct Params` in pipeline.wgsl
/// field-for-field, including padding (WGSL uniform buffers require 16-byte
/// alignment on vec4 boundaries).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct GpuParams {
    pub black: f32,
    pub exposure: f32,
    pub _pad0: f32,
    pub _pad1: f32,

    pub wb: [f32; 4],

    pub cam_to_srgb_0: [f32; 4],
    pub cam_to_srgb_1: [f32; 4],
    pub cam_to_srgb_2: [f32; 4],

    pub width: u32,
    pub height: u32,
    pub cfa_pattern: u32,
    pub black_level: f32,
    
    pub white_level: f32,
    pub _pad2: u32, // Pad elements to align structure size to 112 bytes
    pub _pad3: u32,
    pub _pad4: u32,
}

impl GpuParams {
    pub fn new(
        exposure: &ExposureParams,
        wb_coeffs: [f32; 4],
        cam_to_srgb: [[f32; 3]; 3],
        width: u32,
        height: u32,
        cfa_pattern: u32,
    ) -> Self {
        Self {
            black: exposure.black,
            exposure: exposure.exposure,
            _pad0: 0.0,
            _pad1: 0.0,
            wb: wb_coeffs,
            cam_to_srgb_0: [
                cam_to_srgb[0][0],
                cam_to_srgb[0][1],
                cam_to_srgb[0][2],
                0.0,
            ],
            cam_to_srgb_1: [
                cam_to_srgb[1][0],
                cam_to_srgb[1][1],
                cam_to_srgb[1][2],
                0.0,
            ],
            cam_to_srgb_2: [
                cam_to_srgb[2][0],
                cam_to_srgb[2][1],
                cam_to_srgb[2][2],
                0.0,
            ],
            width,
            height,
            cfa_pattern,
            black_level: 0.0, // raw already normalized to 0..1 on load, see raw_loader.rs
            white_level: 1.0,
            _pad2: 0,
            _pad3: 0,
            _pad4: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_exposure_zero_black_is_identity() {
        let p = ExposureParams {
            black: 0.0,
            exposure: 0.0,
        };
        // white = exp2(0) = 1.0, scale = 1/(1-0) = 1.0
        let s = p.compute_scale();
        assert!((s.scale - 1.0).abs() < 1e-6);
        assert_eq!(s.black, 0.0);
        assert_eq!(p.apply_reference([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn plus_one_ev_doubles_output() {
        let p = ExposureParams {
            black: 0.0,
            exposure: 1.0,
        };
        // white = exp2(-1) = 0.5, scale = 1/0.5 = 2.0
        let s = p.compute_scale();
        assert!((s.scale - 2.0).abs() < 1e-6);
        let out = p.apply_reference([0.25, 0.25, 0.25]);
        assert!((out[0] - 0.5).abs() < 1e-6);
    }
}