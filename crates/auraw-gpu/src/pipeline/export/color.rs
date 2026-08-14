//! Output color-profile resolution and built-in matrix-shaper ICC generation.

use super::super::RenderingIntent;
use super::*;

pub(super) struct ResolvedExportColor {
    pub(super) transform: Option<IccOutputTransform>,
    pub(super) embedded_icc: Option<Vec<u8>>,
    pub(super) srgb: bool,
}

#[derive(Clone, Copy)]
enum IccTransfer {
    Linear,
    Srgb,
}

pub(super) fn built_in_srgb_icc() -> Vec<u8> {
    build_matrix_shaper_icc(
        "sRGB",
        [
            [0.436_074_7, 0.385_064_9, 0.143_080_4],
            [0.222_504_5, 0.716_878_6, 0.060_616_9],
            [0.013_932_2, 0.097_104_5, 0.714_173_3],
        ],
        IccTransfer::Srgb,
    )
}

fn build_matrix_shaper_icc(_name: &str, matrix: [[f32; 3]; 3], transfer: IccTransfer) -> Vec<u8> {
    fn fixed(value: f32) -> [u8; 4] {
        ((value as f64 * 65_536.0).round() as i32).to_be_bytes()
    }
    fn xyz_tag(xyz: [f32; 3]) -> Vec<u8> {
        let mut data = Vec::with_capacity(20);
        data.extend_from_slice(b"XYZ ");
        data.extend_from_slice(&[0; 4]);
        for value in xyz {
            data.extend_from_slice(&fixed(value));
        }
        data
    }
    fn curve_tag(transfer: IccTransfer) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"curv");
        data.extend_from_slice(&[0; 4]);
        match transfer {
            IccTransfer::Linear => {
                data.extend_from_slice(&0u32.to_be_bytes());
            }
            IccTransfer::Srgb => {
                const SAMPLES: u32 = 1024;
                data.extend_from_slice(&SAMPLES.to_be_bytes());
                for index in 0..SAMPLES {
                    let encoded = index as f32 / (SAMPLES - 1) as f32;
                    let linear = if encoded <= 0.04045 {
                        encoded / 12.92
                    } else {
                        ((encoded + 0.055) / 1.055).powf(2.4)
                    };
                    let sample = (linear.clamp(0.0, 1.0) * 65_535.0).round() as u16;
                    data.extend_from_slice(&sample.to_be_bytes());
                }
            }
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        data
    }

    let tags = [
        (*b"wtpt", xyz_tag([0.9642, 1.0, 0.8249])),
        (
            *b"rXYZ",
            xyz_tag([matrix[0][0], matrix[1][0], matrix[2][0]]),
        ),
        (
            *b"gXYZ",
            xyz_tag([matrix[0][1], matrix[1][1], matrix[2][1]]),
        ),
        (
            *b"bXYZ",
            xyz_tag([matrix[0][2], matrix[1][2], matrix[2][2]]),
        ),
        (*b"rTRC", curve_tag(transfer)),
        (*b"gTRC", curve_tag(transfer)),
        (*b"bTRC", curve_tag(transfer)),
    ];

    let table_size = 128usize + 4 + tags.len() * 12;
    let mut offsets = Vec::with_capacity(tags.len());
    let mut cursor = table_size;
    for (_, data) in &tags {
        cursor = (cursor + 3) & !3;
        offsets.push(cursor);
        cursor += data.len();
    }
    let profile_size = cursor;

    let mut profile = vec![0u8; table_size];
    profile[0..4].copy_from_slice(&(profile_size as u32).to_be_bytes());
    profile[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes());
    profile[12..16].copy_from_slice(b"mntr");
    profile[16..20].copy_from_slice(b"RGB ");
    profile[20..24].copy_from_slice(b"XYZ ");
    profile[24..26].copy_from_slice(&2026u16.to_be_bytes());
    profile[26..28].copy_from_slice(&1u16.to_be_bytes());
    profile[28..30].copy_from_slice(&1u16.to_be_bytes());
    profile[36..40].copy_from_slice(b"acsp");
    profile[40..44].copy_from_slice(b"APPL");
    profile[64..68].copy_from_slice(&0u32.to_be_bytes());
    profile[68..72].copy_from_slice(&fixed(0.9642));
    profile[72..76].copy_from_slice(&fixed(1.0));
    profile[76..80].copy_from_slice(&fixed(0.8249));
    profile[80..84].copy_from_slice(b"AuRw");
    profile[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());
    for (index, ((signature, data), offset)) in tags.iter().zip(&offsets).enumerate() {
        let base = 132 + index * 12;
        profile[base..base + 4].copy_from_slice(signature);
        profile[base + 4..base + 8].copy_from_slice(&(*offset as u32).to_be_bytes());
        profile[base + 8..base + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
    }
    for ((_, data), offset) in tags.iter().zip(offsets) {
        while profile.len() < offset {
            profile.push(0);
        }
        profile.extend_from_slice(data);
    }
    profile
}

pub(super) fn resolve_export_color(settings: &ExportSettings) -> Result<ResolvedExportColor> {
    if settings.bit_depth.is_float() {
        return Ok(ResolvedExportColor {
            transform: None,
            embedded_icc: Some(build_matrix_shaper_icc(
                "Linear Rec.2020",
                [
                    [0.673_424_1, 0.165_641_1, 0.125_128_6],
                    [0.279_017_7, 0.675_340_2, 0.045_637_7],
                    [-0.001_930_0, 0.029_978_4, 0.797_333],
                ],
                IccTransfer::Linear,
            )),
            srgb: false,
        });
    }

    match settings.color_profile {
        ExportColorProfile::Srgb => Ok(ResolvedExportColor {
            transform: Some(IccOutputTransform::srgb()),
            embedded_icc: None,
            srgb: true,
        }),
        ExportColorProfile::CustomIcc => {
            let path = settings
                .custom_icc_path
                .as_deref()
                .context("select a custom ICC profile before exporting")?;
            let bytes = fs::read(path)
                .with_context(|| format!("read output ICC profile {}", path.display()))?;
            anyhow::ensure!(
                (132..=64 * 1024 * 1024).contains(&bytes.len()),
                "output ICC profile has an invalid size"
            );
            let transform =
                IccOutputTransform::from_icc(&bytes, RenderingIntent::RelativeColorimetric)
                    .with_context(|| format!("build output transform from {}", path.display()))?;
            Ok(ResolvedExportColor {
                transform: Some(transform),
                embedded_icc: Some(bytes),
                srgb: false,
            })
        }
    }
}
