use anyhow::{anyhow, Context, Result};
use rawloader::{RawImage, RawImageData};
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

pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    let image = rawloader::decode_file(path)
        .with_context(|| format!("rawloader could not decode {}", path.display()))?;

    let raw_pixels = raw_pixels(&image)?;
    let color_indices = color_indices(&image)?;
    let wb_coeffs = white_balance(&image);
    let cam_to_srgb = cam_to_srgb(&image);

    Ok(LoadedRaw {
        width: image.width as u32,
        height: image.height as u32,
        camera_make: if image.clean_make.is_empty() {
            image.make.clone()
        } else {
            image.clean_make.clone()
        },
        camera_model: if image.clean_model.is_empty() {
            image.model.clone()
        } else {
            image.clean_model.clone()
        },
        raw_pixels,
        color_indices,
        wb_coeffs,
        cam_to_srgb,
        black_levels: image.blacklevels.map(|v| v as f32),
        white_levels: image
            .whitelevels
            .map(|v| if v == 0 { 65535.0 } else { v as f32 }),
    })
}

fn raw_pixels(image: &RawImage) -> Result<Vec<u16>> {
    if image.cpp != 1 {
        return Err(anyhow!(
            "unsupported raw layout: expected a single-channel CFA image, got {} components",
            image.cpp
        ));
    }

    match &image.data {
        RawImageData::Integer(data) => Ok(data.clone()),
        RawImageData::Float(data) => {
            let max_value = data
                .iter()
                .copied()
                .filter(|v| v.is_finite())
                .fold(0.0_f32, f32::max)
                .max(1.0);

            Ok(data
                .iter()
                .map(|v| ((v / max_value).clamp(0.0, 1.0) * 65535.0).round() as u16)
                .collect())
        }
    }
}

fn color_indices(image: &RawImage) -> Result<Vec<u8>> {
    let pixels = image
        .width
        .checked_mul(image.height)
        .ok_or_else(|| anyhow!("raw dimensions overflow"))?;

    if !image.cfa.is_valid() {
        return Err(anyhow!(
            "unsupported raw layout: missing CFA for single-channel image"
        ));
    }

    let mut indices = Vec::with_capacity(pixels);
    for y in 0..image.height {
        for x in 0..image.width {
            indices.push(image.cfa.color_at(y, x).min(3) as u8);
        }
    }
    Ok(indices)
}

fn white_balance(image: &RawImage) -> [f32; 4] {
    let mut wb = image.wb_coeffs;
    if wb[..3].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        wb = image.neutralwb();
    }

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

fn cam_to_srgb(image: &RawImage) -> [[f32; 4]; 3] {
    let cam_to_xyz = image.cam_to_xyz_normalized();
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
