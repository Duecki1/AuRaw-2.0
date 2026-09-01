use super::raw_loader::{validate_raw_dimensions, LoadedRaw, RawThumbnail};
use anyhow::{anyhow, Context, Result};
use image::{ColorType, ImageFormat};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[cfg(target_os = "android")]
const MAX_TIFF_DECODE_BYTES: u64 = 768 * 1024 * 1024;
#[cfg(not(target_os = "android"))]
const MAX_TIFF_DECODE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TIFF_IFDS: usize = 32;
const MAX_TIFF_IFD_ENTRIES: u64 = 4096;
const MAX_TIFF_SUBIFDS: usize = 64;
const MIN_TIFF_ICC_BYTES: u64 = 132;
const MAX_TIFF_ICC_BYTES: u64 = 16 * 1024 * 1024;

const REC709_TO_REC2020: [[f32; 3]; 3] = [
    [0.627_403_9, 0.329_283, 0.043_313_1],
    [0.069_097_3, 0.919_540_4, 0.011_362_3],
    [0.016_391_4, 0.088_013_3, 0.895_595_3],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TiffContainerKind {
    Sensor,
    Raster,
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }
}

pub(super) fn is_tiff_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
        })
}

pub(super) fn inspect_tiff_container(path: &Path) -> Result<TiffContainerKind> {
    let mut file = File::open(path).with_context(|| format!("open TIFF {}", path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("inspect TIFF {}", path.display()))?
        .len();
    let mut header = [0u8; 16];
    file.read_exact(&mut header[..8])
        .with_context(|| format!("read TIFF header {}", path.display()))?;
    let order = match &header[..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err(anyhow!("{} is not a TIFF container", path.display())),
    };
    let magic = order.u16([header[2], header[3]]);
    let (big_tiff, first_ifd) = match magic {
        42 => (
            false,
            u64::from(order.u32(header[4..8].try_into().unwrap())),
        ),
        43 => {
            file.read_exact(&mut header[8..16])
                .with_context(|| format!("read BigTIFF header {}", path.display()))?;
            let offset_size = order.u16([header[4], header[5]]);
            let reserved = order.u16([header[6], header[7]]);
            anyhow::ensure!(
                offset_size == 8 && reserved == 0,
                "unsupported BigTIFF header in {}",
                path.display()
            );
            (true, order.u64(header[8..16].try_into().unwrap()))
        }
        _ => return Err(anyhow!("{} has an invalid TIFF magic", path.display())),
    };

    let mut pending = vec![first_ifd];
    let mut visited = std::collections::HashSet::new();
    let mut inspected = 0usize;
    while let Some(ifd_offset) = pending.pop() {
        if ifd_offset == 0 || !visited.insert(ifd_offset) {
            continue;
        }
        anyhow::ensure!(inspected < MAX_TIFF_IFDS, "TIFF contains too many IFDs");
        inspected += 1;
        anyhow::ensure!(ifd_offset < file_len, "TIFF IFD offset is outside the file");
        file.seek(SeekFrom::Start(ifd_offset))?;

        let entry_count = if big_tiff {
            read_u64(&mut file, order)?
        } else {
            u64::from(read_u16(&mut file, order)?)
        };
        anyhow::ensure!(
            entry_count <= MAX_TIFF_IFD_ENTRIES,
            "TIFF IFD contains too many entries"
        );

        let entry_size = if big_tiff { 20u64 } else { 12u64 };
        let count_size = if big_tiff { 8u64 } else { 2u64 };
        let entries_start = ifd_offset
            .checked_add(count_size)
            .context("TIFF IFD offset overflow")?;
        let entries_bytes = entry_count
            .checked_mul(entry_size)
            .context("TIFF IFD size overflow")?;
        let next_pos = entries_start
            .checked_add(entries_bytes)
            .context("TIFF IFD size overflow")?;
        let next_width = if big_tiff { 8u64 } else { 4u64 };
        anyhow::ensure!(
            next_pos
                .checked_add(next_width)
                .is_some_and(|end| end <= file_len),
            "TIFF IFD extends outside the file"
        );

        for entry_index in 0..entry_count {
            let entry_offset = entries_start + entry_index * entry_size;
            file.seek(SeekFrom::Start(entry_offset))?;
            let tag = read_u16(&mut file, order)?;
            let field_type = read_u16(&mut file, order)?;
            let count = if big_tiff {
                read_u64(&mut file, order)?
            } else {
                u64::from(read_u32(&mut file, order)?)
            };
            let value_field_offset = file.stream_position()?;
            let value_or_offset = if big_tiff {
                read_u64(&mut file, order)?
            } else {
                u64::from(read_u32(&mut file, order)?)
            };
            let field = TiffFieldRef {
                order,
                big_tiff,
                field_type,
                count,
                value_field_offset,
                value_or_offset,
                file_len,
            };

            if matches!(tag, 33421 | 33422) {
                return Ok(TiffContainerKind::Sensor);
            }

            if tag == 262 && count == 1 {
                if let Some(value) = scalar_u64(&mut file, field)? {
                    if matches!(value, 32803 | 34892) {
                        return Ok(TiffContainerKind::Sensor);
                    }
                }
            }

            if tag == 330 && pending.len() < MAX_TIFF_SUBIFDS {
                let offsets = integer_values(&mut file, field, MAX_TIFF_SUBIFDS - pending.len())?;
                pending.extend(offsets.into_iter().filter(|offset| *offset != 0));
            }
        }

        file.seek(SeekFrom::Start(next_pos))?;
        let next_ifd = if big_tiff {
            read_u64(&mut file, order)?
        } else {
            u64::from(read_u32(&mut file, order)?)
        };
        if next_ifd != 0 {
            pending.push(next_ifd);
        }
    }

    Ok(TiffContainerKind::Raster)
}

fn read_u16(reader: &mut File, order: ByteOrder) -> Result<u16> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(order.u16(bytes))
}

fn read_u32(reader: &mut File, order: ByteOrder) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(order.u32(bytes))
}

fn read_u64(reader: &mut File, order: ByteOrder) -> Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(order.u64(bytes))
}

fn field_width(field_type: u16) -> Option<u64> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct TiffFieldRef {
    order: ByteOrder,
    big_tiff: bool,
    field_type: u16,
    count: u64,
    value_field_offset: u64,
    value_or_offset: u64,
    file_len: u64,
}

impl TiffFieldRef {
    fn data_offset(self, total_bytes: u64) -> u64 {
        let inline_bytes = if self.big_tiff { 8 } else { 4 };
        if total_bytes <= inline_bytes {
            self.value_field_offset
        } else {
            self.value_or_offset
        }
    }
}

fn scalar_u64(file: &mut File, field: TiffFieldRef) -> Result<Option<u64>> {
    Ok(integer_values(file, field, 1)?.into_iter().next())
}

fn integer_values(file: &mut File, field: TiffFieldRef, limit: usize) -> Result<Vec<u64>> {
    let TiffFieldRef {
        order,
        field_type,
        count,
        file_len,
        ..
    } = field;

    let Some(width) = field_width(field_type) else {
        return Ok(Vec::new());
    };
    if !matches!(field_type, 1 | 3 | 4 | 13 | 16 | 18) || count == 0 {
        return Ok(Vec::new());
    }
    let total = width.checked_mul(count).context("TIFF tag size overflow")?;
    let data_offset = field.data_offset(total);
    anyhow::ensure!(
        data_offset
            .checked_add(total)
            .is_some_and(|end| end <= file_len),
        "TIFF tag data extends outside the file"
    );
    let current = file.stream_position()?;
    file.seek(SeekFrom::Start(data_offset))?;
    let value_count = usize::try_from(count).unwrap_or(usize::MAX).min(limit);
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        let value = match field_type {
            1 => {
                let mut byte = [0u8; 1];
                file.read_exact(&mut byte)?;
                u64::from(byte[0])
            }
            3 => u64::from(read_u16(file, order)?),
            4 | 13 => u64::from(read_u32(file, order)?),
            16 | 18 => read_u64(file, order)?,
            _ => unreachable!(),
        };
        values.push(value);
    }
    file.seek(SeekFrom::Start(current))?;
    Ok(values)
}

pub(super) fn load_raster_tiff(path: &Path) -> Result<LoadedRaw> {
    let (width, height, rgb) = decode_scene_linear_rec2020(path)?;
    LoadedRaw::from_scene_linear_rec2020(width, height, rgb)
}

fn decode_scene_linear_rec2020(path: &Path) -> Result<(u32, u32, Vec<f32>)> {
    let image = decode_tiff(path)?;
    let width = image.width();
    let height = image.height();
    validate_raw_dimensions(width, height)?;
    let source_color = image.color();
    let source_is_float = matches!(source_color, ColorType::Rgb32F | ColorType::Rgba32F);
    let mut rgb = image.into_rgb32f().into_raw();
    if rgb.par_iter().any(|value| !value.is_finite()) {
        return Err(anyhow!("TIFF contains NaN or infinity"));
    }

    if let Some(icc) = read_embedded_icc_profile(path)? {
        super::color_profile::convert_embedded_icc_rgb_to_rec2020(&icc, &mut rgb)
            .with_context(|| format!("apply embedded TIFF ICC profile from {}", path.display()))?;
        if rgb.par_iter().any(|value| !value.is_finite()) {
            return Err(anyhow!(
                "embedded TIFF ICC conversion produced NaN or infinity"
            ));
        }
    } else if source_is_float {
    } else {
        rgb.par_chunks_exact_mut(3).for_each(|pixel| {
            let r = srgb_to_linear(pixel[0]);
            let g = srgb_to_linear(pixel[1]);
            let b = srgb_to_linear(pixel[2]);
            pixel[0] = REC709_TO_REC2020[0][0] * r
                + REC709_TO_REC2020[0][1] * g
                + REC709_TO_REC2020[0][2] * b;
            pixel[1] = REC709_TO_REC2020[1][0] * r
                + REC709_TO_REC2020[1][1] * g
                + REC709_TO_REC2020[1][2] * b;
            pixel[2] = REC709_TO_REC2020[2][0] * r
                + REC709_TO_REC2020[2][1] * g
                + REC709_TO_REC2020[2][2] * b;
        });
    }

    Ok((width, height, rgb))
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn load_raster_tiff_dimensions(path: &Path) -> Result<[u32; 2]> {
    let file = File::open(path).with_context(|| format!("open TIFF {}", path.display()))?;
    let mut reader = image::ImageReader::with_format(BufReader::new(file), ImageFormat::Tiff);
    configure_limits(&mut reader);
    let (width, height) = reader
        .into_dimensions()
        .with_context(|| format!("inspect TIFF dimensions {}", path.display()))?;
    validate_raw_dimensions(width, height)?;
    Ok([width, height])
}

pub(super) fn load_raster_tiff_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    anyhow::ensure!(maximum_edge > 0, "thumbnail edge must be non-zero");
    let (width, height, rgb) = decode_scene_linear_rec2020(path)?;
    let image = image::DynamicImage::ImageRgb32F(
        image::Rgb32FImage::from_raw(width, height, rgb)
            .ok_or_else(|| anyhow!("TIFF RGB buffer size does not match its dimensions"))?,
    );
    let image = crate::thumbnail_cache::downscale_to_fit(image, maximum_edge);
    let (width, height) = (image.width(), image.height());
    let rgb = image.into_rgb32f().into_raw();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in rgb.chunks_exact(3) {
        let encoded =
            super::color_profile::display_linear_rec2020_to_srgb([pixel[0], pixel[1], pixel[2]]);
        for value in encoded {
            rgba.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    Ok(RawThumbnail {
        width,
        height,
        rgba,
    })
}

fn read_embedded_icc_profile(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = File::open(path).with_context(|| format!("open TIFF {}", path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("inspect TIFF {}", path.display()))?
        .len();
    let mut header = [0u8; 16];
    file.read_exact(&mut header[..8])
        .with_context(|| format!("read TIFF header {}", path.display()))?;
    let order = match &header[..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err(anyhow!("{} is not a TIFF container", path.display())),
    };
    let magic = order.u16([header[2], header[3]]);
    let (big_tiff, first_ifd) = match magic {
        42 => (
            false,
            u64::from(order.u32(header[4..8].try_into().unwrap())),
        ),
        43 => {
            file.read_exact(&mut header[8..16])
                .with_context(|| format!("read BigTIFF header {}", path.display()))?;
            let offset_size = order.u16([header[4], header[5]]);
            let reserved = order.u16([header[6], header[7]]);
            anyhow::ensure!(
                offset_size == 8 && reserved == 0,
                "unsupported BigTIFF header in {}",
                path.display()
            );
            (true, order.u64(header[8..16].try_into().unwrap()))
        }
        _ => return Err(anyhow!("{} has an invalid TIFF magic", path.display())),
    };

    let mut pending = vec![first_ifd];
    let mut visited = std::collections::HashSet::new();
    let mut inspected = 0usize;
    while let Some(ifd_offset) = pending.pop() {
        if ifd_offset == 0 || !visited.insert(ifd_offset) {
            continue;
        }
        anyhow::ensure!(inspected < MAX_TIFF_IFDS, "TIFF contains too many IFDs");
        inspected += 1;
        anyhow::ensure!(ifd_offset < file_len, "TIFF IFD offset is outside the file");
        file.seek(SeekFrom::Start(ifd_offset))?;

        let entry_count = if big_tiff {
            read_u64(&mut file, order)?
        } else {
            u64::from(read_u16(&mut file, order)?)
        };
        anyhow::ensure!(
            entry_count <= MAX_TIFF_IFD_ENTRIES,
            "TIFF IFD contains too many entries"
        );
        let entry_size = if big_tiff { 20u64 } else { 12u64 };
        let count_size = if big_tiff { 8u64 } else { 2u64 };
        let entries_start = ifd_offset
            .checked_add(count_size)
            .context("TIFF IFD offset overflow")?;
        let entries_bytes = entry_count
            .checked_mul(entry_size)
            .context("TIFF IFD size overflow")?;
        let next_pos = entries_start
            .checked_add(entries_bytes)
            .context("TIFF IFD size overflow")?;
        let next_width = if big_tiff { 8u64 } else { 4u64 };
        anyhow::ensure!(
            next_pos
                .checked_add(next_width)
                .is_some_and(|end| end <= file_len),
            "TIFF IFD extends outside the file"
        );

        for entry_index in 0..entry_count {
            let entry_offset = entries_start + entry_index * entry_size;
            file.seek(SeekFrom::Start(entry_offset))?;
            let tag = read_u16(&mut file, order)?;
            let field_type = read_u16(&mut file, order)?;
            let count = if big_tiff {
                read_u64(&mut file, order)?
            } else {
                u64::from(read_u32(&mut file, order)?)
            };
            let value_field_offset = file.stream_position()?;
            let value_or_offset = if big_tiff {
                read_u64(&mut file, order)?
            } else {
                u64::from(read_u32(&mut file, order)?)
            };
            let field = TiffFieldRef {
                order,
                big_tiff,
                field_type,
                count,
                value_field_offset,
                value_or_offset,
                file_len,
            };

            if tag == 34675 {
                anyhow::ensure!(
                    matches!(field_type, 1 | 7),
                    "TIFF ICC profile tag has unsupported field type {field_type}"
                );
                anyhow::ensure!(
                    (MIN_TIFF_ICC_BYTES..=MAX_TIFF_ICC_BYTES).contains(&count),
                    "TIFF ICC profile has an invalid size"
                );
                let data_offset = field.data_offset(count);
                anyhow::ensure!(
                    data_offset
                        .checked_add(count)
                        .is_some_and(|end| end <= file_len),
                    "TIFF ICC profile extends outside the file"
                );
                file.seek(SeekFrom::Start(data_offset))?;
                let mut bytes =
                    vec![0u8; usize::try_from(count).context("TIFF ICC profile size overflow")?];
                file.read_exact(&mut bytes)?;
                return Ok(Some(normalize_icc_profile(bytes)?));
            }

            if tag == 330 && pending.len() < MAX_TIFF_SUBIFDS {
                let offsets = integer_values(&mut file, field, MAX_TIFF_SUBIFDS - pending.len())?;
                pending.extend(offsets.into_iter().filter(|offset| *offset != 0));
            }
        }

        file.seek(SeekFrom::Start(next_pos))?;
        let next_ifd = if big_tiff {
            read_u64(&mut file, order)?
        } else {
            u64::from(read_u32(&mut file, order)?)
        };
        if next_ifd != 0 {
            pending.push(next_ifd);
        }
    }
    Ok(None)
}

fn normalize_icc_profile(mut bytes: Vec<u8>) -> Result<Vec<u8>> {
    anyhow::ensure!(
        bytes.len() >= MIN_TIFF_ICC_BYTES as usize && &bytes[36..40] == b"acsp",
        "TIFF ICC profile has an invalid header"
    );
    let declared = u32::from_be_bytes(
        bytes[0..4]
            .try_into()
            .map_err(|_| anyhow!("TIFF ICC profile has an invalid size field"))?,
    ) as usize;
    anyhow::ensure!(
        (MIN_TIFF_ICC_BYTES as usize..=bytes.len()).contains(&declared),
        "TIFF ICC profile declares an invalid size"
    );
    bytes.truncate(declared);
    Ok(bytes)
}

fn decode_tiff(path: &Path) -> Result<image::DynamicImage> {
    let file = File::open(path).with_context(|| format!("open TIFF {}", path.display()))?;
    let mut reader = image::ImageReader::with_format(BufReader::new(file), ImageFormat::Tiff);
    configure_limits(&mut reader);
    let image = reader
        .decode()
        .with_context(|| format!("decode TIFF {} in process", path.display()))?;
    let pixels = validate_raw_dimensions(image.width(), image.height())? as u64;
    anyhow::ensure!(
        pixels.saturating_mul(16) <= MAX_TIFF_DECODE_BYTES,
        "TIFF {} requires too much decoded memory for this platform",
        path.display()
    );
    Ok(image)
}

fn configure_limits<R: std::io::BufRead + Seek>(reader: &mut image::ImageReader<R>) {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(super::raw_loader::MAX_RAW_EDGE);
    limits.max_image_height = Some(super::raw_loader::MAX_RAW_EDGE);
    limits.max_alloc = Some(MAX_TIFF_DECODE_BYTES);
    reader.limits(limits);
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn cfa_photometric_tiff_routes_to_sensor_loader() {
        let path = std::env::temp_dir().join(format!(
            "calibraw-sensor-tiff-{}-{}.tif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&262u16.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&32803u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            inspect_tiff_container(&path).unwrap(),
            TiffContainerKind::Sensor
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rendered_rgb_with_copied_color_matrix_metadata_stays_on_raster_path() {
        let path = std::env::temp_dir().join(format!(
            "calibraw-rendered-metadata-tiff-{}-{}.tif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());

        bytes.extend_from_slice(&262u16.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());

        bytes.extend_from_slice(&50706u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 6, 0, 0]);

        bytes.extend_from_slice(&50721u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(
            inspect_tiff_container(&path).unwrap(),
            TiffContainerKind::Raster
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn icc_payload_is_trimmed_to_its_declared_profile_size() {
        let mut bytes = vec![0u8; 148];
        bytes[0..4].copy_from_slice(&132u32.to_be_bytes());
        bytes[36..40].copy_from_slice(b"acsp");
        let normalized = normalize_icc_profile(bytes).unwrap();
        assert_eq!(normalized.len(), 132);
    }

    #[test]
    fn icc_payload_rejects_a_declared_size_beyond_the_tag() {
        let mut bytes = vec![0u8; 132];
        bytes[0..4].copy_from_slice(&4096u32.to_be_bytes());
        bytes[36..40].copy_from_slice(b"acsp");
        assert!(normalize_icc_profile(bytes).is_err());
    }

    #[test]
    fn integer_tiff_decodes_to_scene_linear_rec2020() {
        let path = std::env::temp_dir().join(format!(
            "calibraw-raster-tiff-{}-{}.TIFF",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let image = ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(1, 1, vec![65535, 0, 0]).unwrap();
        image.save_with_format(&path, ImageFormat::Tiff).unwrap();
        assert_eq!(
            inspect_tiff_container(&path).unwrap(),
            TiffContainerKind::Raster
        );
        let loaded = load_raster_tiff(&path).unwrap();
        assert!(loaded.is_pre_demosaiced_raster());
        let rgb = loaded.scene_linear_raster().unwrap();
        assert!((rgb[0] - REC709_TO_REC2020[0][0]).abs() < 2e-4);
        assert!((rgb[1] - REC709_TO_REC2020[1][0]).abs() < 2e-4);
        assert!((rgb[2] - REC709_TO_REC2020[2][0]).abs() < 2e-4);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn raster_tiff_thumbnail_preserves_black_and_white_endpoints() {
        let path = std::env::temp_dir().join(format!(
            "calibraw-raster-thumb-{}-{}.tif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let image =
            ImageBuffer::<Rgb<u16>, Vec<u16>>::from_raw(2, 1, vec![0, 0, 0, 65535, 65535, 65535])
                .unwrap();
        image.save_with_format(&path, ImageFormat::Tiff).unwrap();

        let thumbnail = load_raster_tiff_thumbnail(&path, 512).unwrap();
        assert_eq!([thumbnail.width, thumbnail.height], [2, 1]);
        assert_eq!(&thumbnail.rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&thumbnail.rgba[4..8], &[255, 255, 255, 255]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reads_embedded_icc_profile_tag() {
        let path = std::env::temp_dir().join(format!(
            "calibraw-icc-tiff-{}-{}.tif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut icc = vec![0u8; 132];
        icc[0..4].copy_from_slice(&132u32.to_be_bytes());
        icc[8] = 4;
        icc[12..16].copy_from_slice(b"spac");
        icc[16..20].copy_from_slice(b"RGB ");
        icc[20..24].copy_from_slice(b"XYZ ");
        icc[36..40].copy_from_slice(b"acsp");

        let payload_offset = 8 + 2 + 12 + 4;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&34675u16.to_le_bytes());
        bytes.extend_from_slice(&7u16.to_le_bytes());
        bytes.extend_from_slice(&(icc.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(payload_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&icc);
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(read_embedded_icc_profile(&path).unwrap(), Some(icc));
        let _ = std::fs::remove_file(path);
    }
}
