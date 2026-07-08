use anyhow::{anyhow, Result};
use std::ffi::{CStr, CString};
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

pub struct LoadedRaw {
    pub raw_pixels: Vec<f32>,
    pub width: u32,
    pub height: u32,

    pub cfa_pattern: u32,

    pub wb_coeffs: [f32; 4],

    pub cam_to_srgb: [[f32; 3]; 3],

    pub camera_make: String,
    pub camera_model: String,
}

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

    [
        [cof_a * inv_det, cof_d * inv_det, cof_g * inv_det],
        [cof_b * inv_det, cof_e * inv_det, cof_h * inv_det],
        [cof_c * inv_det, cof_f * inv_det, cof_i * inv_det],
    ]
}

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

fn decode_cfa_pattern(filters: u32) -> u32 {
    let fc = |x: u32, y: u32| -> u32 {
        (filters >> ((((y << 1) & 14) + (x & 1)) << 1)) & 3
    };
    let tl = fc(0, 0);
    let tr = fc(1, 0);
    let bl = fc(0, 1);
    let br = fc(1, 1);
    match (tl, tr, bl, br) {
        (0, 1, 1, 2) => 0,
        (2, 1, 1, 0) => 1,
        (1, 0, 2, 1) => 2,
        (1, 2, 0, 1) => 3,
        _ => {
            log::warn!(
                "Unknown CFA layout (tl={tl}, tr={tr}, bl={bl}, br={br}, filters=0x{filters:08x}); \
                 defaulting to RGGB."
            );
            0
        }
    }
}

fn libraw_fc(filters: u32, x: u32, y: u32) -> usize {
    let x = x as i32;
    let y = y as i32;
    ((filters >> ((((y << 1) & 14) + (x & 1)) << 1)) & 3) as usize
}

pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    log::info!("LibRaw: opening {}", path.display());

    let handle = unsafe { libraw_init(0) };
    if handle.is_null() {
        return Err(anyhow!("libraw_init failed"));
    }

    let c_path = CString::new(path.to_str().ok_or_else(|| anyhow!("Invalid path string"))?)?;
    let ret = unsafe { libraw_open_file(handle, c_path.as_ptr()) };
    if ret != 0 {
        unsafe { libraw_close(handle) };
        return Err(anyhow!("libraw_open_file failed with code {}", ret));
    }

    let ret = unsafe { libraw_unpack(handle) };
    if ret != 0 {
        unsafe { libraw_close(handle) };
        return Err(anyhow!("libraw_unpack failed with code {}", ret));
    }

    let imgdata = unsafe { &*handle };

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

    let raw_ptr = imgdata.rawdata.raw_image;
    if raw_ptr.is_null() {
        unsafe { libraw_close(handle) };
        return Err(anyhow!(
            "LibRaw returned null raw_image pointer. File might be linear DNG or unsupported."
        ));
    }

    let base_black = imgdata.color.black as f32;
    let maximum = imgdata.color.maximum as f32;
    
    let cblack = [
        imgdata.color.cblack[0] as f32,
        imgdata.color.cblack[1] as f32,
        imgdata.color.cblack[2] as f32,
        imgdata.color.cblack[3] as f32,
    ];

    let norm_scale = [
        if maximum > cblack[0] { 1.0 / (maximum - cblack[0]) } else { 1.0 },
        if maximum > cblack[1] { 1.0 / (maximum - cblack[1]) } else { 1.0 },
        if maximum > cblack[2] { 1.0 / (maximum - cblack[2]) } else { 1.0 },
        if maximum > cblack[3] { 1.0 / (maximum - cblack[3]) } else { 1.0 },
    ];

    let norm_scale_fallback = if maximum > base_black { 1.0 / (maximum - base_black) } else { 1.0 };

    let filters = imgdata.idata.filters;
    let raw_pixels: Vec<f32> = (0..height)
        .flat_map(|y| {
            let ry = (y + top_margin) as usize;
            (0..width).map(move |x| {
                let rx = (x + left_margin) as usize;
                let idx = ry * raw_width as usize + rx;
                let v = unsafe { *raw_ptr.add(idx) } as f32;

                let c = if filters == 9 {
                    (imgdata.idata.xtrans[ry % 6][rx % 6] as u8) as usize
                } else {
                    libraw_fc(filters, x, y)
                };
                let black = if cblack[c] > 0.0 { cblack[c] } else { base_black };
                let scale = if cblack[c] > 0.0 { norm_scale[c] } else { norm_scale_fallback };
                
                (v - black) * scale
            })
        })
        .collect();

    let cam_mul = imgdata.color.cam_mul;
    let g1 = if cam_mul[1] > 0.0 && cam_mul[1].is_finite() { cam_mul[1] } else { 1.0 };
    let g2 = if cam_mul[3] > 0.0 && cam_mul[3].is_finite() { cam_mul[3] } else { g1 };
    let wb_norm = [cam_mul[0] / g1, 1.0, cam_mul[2] / g1, g2 / g1];

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
        normalize_rows(unnormalized)
    };

    let cfa_pattern = decode_cfa_pattern(imgdata.idata.filters);

    let make_str = unsafe { CStr::from_ptr(imgdata.idata.make.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    let model_str = unsafe { CStr::from_ptr(imgdata.idata.model.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();

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