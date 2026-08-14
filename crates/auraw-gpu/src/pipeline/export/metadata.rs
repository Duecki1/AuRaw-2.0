//! Export metadata encoding shared by PNG, JPEG, and TIFF output.

use super::*;

pub(super) fn add_png_text_metadata<W: Write>(
    encoder: &mut png::Encoder<'_, W>,
    metadata: &ExportMetadata,
    output_width: u32,
    output_height: u32,
) -> Result<()> {
    encoder
        .add_itxt_chunk("Software".to_owned(), "AuRaw 2.0".to_owned())
        .context("write PNG software metadata")?;
    if let Some(source) = metadata
        .source_file_name
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        encoder
            .add_itxt_chunk("Source".to_owned(), source.to_owned())
            .context("write PNG source metadata")?;
    }
    let camera = joined_metadata_label(&metadata.camera_make, &metadata.camera_model);
    if !camera.is_empty() {
        encoder
            .add_itxt_chunk("Camera".to_owned(), camera)
            .context("write PNG camera metadata")?;
    }
    let lens = joined_metadata_label(&metadata.lens_make, &metadata.lens_model);
    if !lens.is_empty() {
        encoder
            .add_itxt_chunk("Lens".to_owned(), lens)
            .context("write PNG lens metadata")?;
    }
    if metadata.focal_length.is_finite() && metadata.focal_length > 0.0 {
        encoder
            .add_itxt_chunk(
                "Focal length".to_owned(),
                format!("{:.1} mm", metadata.focal_length),
            )
            .context("write PNG focal-length metadata")?;
    }
    if metadata.aperture.is_finite() && metadata.aperture > 0.0 {
        encoder
            .add_itxt_chunk("Aperture".to_owned(), format!("f/{:.1}", metadata.aperture))
            .context("write PNG aperture metadata")?;
    }
    if metadata.focus_distance.is_finite() && metadata.focus_distance > 0.0 {
        encoder
            .add_itxt_chunk(
                "Focus distance".to_owned(),
                format!("{:.2} m", metadata.focus_distance),
            )
            .context("write PNG focus-distance metadata")?;
    }
    if metadata.iso_speed.is_finite() && metadata.iso_speed > 0.0 {
        encoder
            .add_itxt_chunk("ISO speed".to_owned(), format!("{:.0}", metadata.iso_speed))
            .context("write PNG ISO metadata")?;
    }
    if metadata.shutter_seconds.is_finite() && metadata.shutter_seconds > 0.0 {
        encoder
            .add_itxt_chunk(
                "Exposure time".to_owned(),
                format_exposure_time(metadata.shutter_seconds),
            )
            .context("write PNG exposure-time metadata")?;
    }
    if !metadata.artist.trim().is_empty() {
        encoder
            .add_itxt_chunk("Artist".to_owned(), metadata.artist.trim().to_owned())
            .context("write PNG artist metadata")?;
    }
    if !metadata.description.trim().is_empty() {
        encoder
            .add_itxt_chunk(
                "Image description".to_owned(),
                metadata.description.trim().to_owned(),
            )
            .context("write PNG image-description metadata")?;
    }
    encoder
        .add_itxt_chunk(
            "Original dimensions".to_owned(),
            format!("{}x{}", metadata.source_width, metadata.source_height),
        )
        .context("write original dimensions metadata")?;
    encoder
        .add_itxt_chunk(
            "Export dimensions".to_owned(),
            format!("{output_width}x{output_height}"),
        )
        .context("write export dimensions metadata")?;
    encoder
        .add_itxt_chunk("Orientation".to_owned(), "1 (normal)".to_owned())
        .context("write PNG orientation metadata")?;
    Ok(())
}

fn format_exposure_time(seconds: f32) -> String {
    if seconds > 0.0 && seconds < 1.0 {
        let reciprocal = (1.0 / seconds).round().max(1.0);
        if ((1.0 / reciprocal) - seconds).abs() <= seconds * 0.02 {
            return format!("1/{reciprocal:.0} s");
        }
    }
    format!("{seconds:.4} s")
}

fn joined_metadata_label(make: &str, model: &str) -> String {
    match (make.trim(), model.trim()) {
        ("", "") => String::new(),
        ("", model) => model.to_owned(),
        (make, "") => make.to_owned(),
        (make, model) if model.starts_with(make) => model.to_owned(),
        (make, model) => format!("{make} {model}"),
    }
}

fn export_metadata_description(metadata: &ExportMetadata) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(source) = metadata
        .source_file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Processed from {source}"));
    } else {
        parts.push("Processed from a RAW image".to_owned());
    }
    if metadata.source_width > 0 && metadata.source_height > 0 {
        parts.push(format!(
            "original dimensions {}x{}",
            metadata.source_width, metadata.source_height
        ));
    }
    parts.push("exported by AuRaw 2.0".to_owned());
    parts.join("; ")
}

pub(super) fn combined_image_description(metadata: &ExportMetadata) -> String {
    let export_description = export_metadata_description(metadata);
    match metadata.description.trim() {
        "" => export_description,
        original => format!("{original}; {export_description}"),
    }
}

#[derive(Clone)]
enum ExifValue {
    Short(u16),
    Long(u32),
    Ascii(Vec<u8>),
    Rational(u32, u32),
    Undefined(Vec<u8>),
}

#[derive(Clone)]
struct ExifEntry {
    tag: u16,
    value: ExifValue,
}

fn nul_terminated_exif_ascii(value: &str) -> Vec<u8> {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii() && character != '\0' {
                character as u8
            } else {
                b'?'
            }
        })
        .collect::<Vec<_>>();
    output.push(0);
    output
}

fn exif_rational(value: f32) -> Option<(u32, u32)> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let denominator = 10_000u32;
    let numerator = (f64::from(value) * f64::from(denominator))
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32;
    let divisor = greatest_common_divisor(numerator, denominator);
    Some((numerator / divisor, denominator / divisor))
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn exif_value_parts(value: &ExifValue) -> (u16, u32, Vec<u8>) {
    match value {
        ExifValue::Short(value) => (3, 1, value.to_le_bytes().to_vec()),
        ExifValue::Long(value) => (4, 1, value.to_le_bytes().to_vec()),
        ExifValue::Ascii(value) => (2, value.len() as u32, value.clone()),
        ExifValue::Rational(numerator, denominator) => {
            let mut bytes = Vec::with_capacity(8);
            bytes.extend_from_slice(&numerator.to_le_bytes());
            bytes.extend_from_slice(&(*denominator).max(1).to_le_bytes());
            (5, 1, bytes)
        }
        ExifValue::Undefined(value) => (7, value.len() as u32, value.clone()),
    }
}

fn encoded_ifd_block_len(entries: &[ExifEntry]) -> u32 {
    let directory_len = 2usize
        .saturating_add(entries.len().saturating_mul(12))
        .saturating_add(4);
    let data_len = entries
        .iter()
        .map(|entry| {
            let (_, _, bytes) = exif_value_parts(&entry.value);
            if bytes.len() <= 4 {
                0
            } else {
                bytes.len() + (bytes.len() & 1)
            }
        })
        .sum::<usize>();
    u32::try_from(directory_len.saturating_add(data_len)).unwrap_or(u32::MAX)
}

fn encode_ifd_block(entries: &[ExifEntry], ifd_offset: u32) -> Vec<u8> {
    let directory_len = 2usize + entries.len() * 12 + 4;
    let data_offset = ifd_offset.saturating_add(directory_len as u32);
    let mut directory = Vec::with_capacity(directory_len);
    let mut data = Vec::new();
    directory.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    for entry in entries {
        directory.extend_from_slice(&entry.tag.to_le_bytes());
        let (field_type, count, bytes) = exif_value_parts(&entry.value);
        directory.extend_from_slice(&field_type.to_le_bytes());
        directory.extend_from_slice(&count.to_le_bytes());
        if bytes.len() <= 4 {
            directory.extend_from_slice(&bytes);
            directory.resize(directory.len() + 4 - bytes.len(), 0);
        } else {
            let value_offset = data_offset.saturating_add(data.len() as u32);
            directory.extend_from_slice(&value_offset.to_le_bytes());
            data.extend_from_slice(&bytes);
            if data.len() & 1 != 0 {
                data.push(0);
            }
        }
    }
    directory.extend_from_slice(&0u32.to_le_bytes());
    directory.extend_from_slice(&data);
    directory
}

/// Builds a compact, standards-shaped TIFF/EXIF payload used by both JPEG's
/// APP1 segment and PNG's eXIf chunk. The output pixels have already been
/// physically oriented, so Orientation is always normalized to 1.
pub(super) fn build_exif_payload(metadata: &ExportMetadata, output_width: u32, output_height: u32) -> Vec<u8> {
    let mut ifd0_entries = vec![
        ExifEntry {
            tag: 0x0100,
            value: ExifValue::Long(output_width),
        },
        ExifEntry {
            tag: 0x0101,
            value: ExifValue::Long(output_height),
        },
        ExifEntry {
            tag: 0x010e,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&combined_image_description(
                metadata,
            ))),
        },
        ExifEntry {
            tag: 0x0112,
            value: ExifValue::Short(1),
        },
        ExifEntry {
            tag: 0x0131,
            value: ExifValue::Ascii(nul_terminated_exif_ascii("AuRaw 2.0")),
        },
    ];
    if !metadata.camera_make.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x010f,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.camera_make)),
        });
    }
    if !metadata.camera_model.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x0110,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.camera_model)),
        });
    }
    if !metadata.artist.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x013b,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.artist)),
        });
    }

    let mut exif_entries = vec![
        ExifEntry {
            tag: 0x9000,
            value: ExifValue::Undefined(b"0232".to_vec()),
        },
        ExifEntry {
            tag: 0xa002,
            value: ExifValue::Long(output_width),
        },
        ExifEntry {
            tag: 0xa003,
            value: ExifValue::Long(output_height),
        },
    ];
    if let Some((numerator, denominator)) = exif_rational(metadata.shutter_seconds) {
        exif_entries.push(ExifEntry {
            tag: 0x829a,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if metadata.iso_speed.is_finite() && metadata.iso_speed > 0.0 {
        let iso = metadata.iso_speed.round().clamp(1.0, u32::MAX as f32) as u32;
        exif_entries.push(ExifEntry {
            tag: 0x8827,
            value: if iso <= u32::from(u16::MAX) {
                ExifValue::Short(iso as u16)
            } else {
                ExifValue::Long(iso)
            },
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.aperture) {
        exif_entries.push(ExifEntry {
            tag: 0x829d,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.focal_length) {
        exif_entries.push(ExifEntry {
            tag: 0x920a,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.focus_distance) {
        exif_entries.push(ExifEntry {
            tag: 0x9206,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if !metadata.lens_make.trim().is_empty() {
        exif_entries.push(ExifEntry {
            tag: 0xa433,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.lens_make)),
        });
    }
    if !metadata.lens_model.trim().is_empty() {
        exif_entries.push(ExifEntry {
            tag: 0xa434,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.lens_model)),
        });
    }
    let mut user_comment = b"ASCII\0\0\0".to_vec();
    user_comment
        .extend_from_slice(&nul_terminated_exif_ascii(&combined_image_description(metadata))[..]);
    exif_entries.push(ExifEntry {
        tag: 0x9286,
        value: ExifValue::Undefined(user_comment),
    });

    ifd0_entries.sort_by_key(|entry| entry.tag);
    exif_entries.sort_by_key(|entry| entry.tag);

    // Adding the ExifIFD pointer changes IFD0's directory length, so include a
    // placeholder before calculating the nested IFD's final TIFF-relative offset.
    ifd0_entries.push(ExifEntry {
        tag: 0x8769,
        value: ExifValue::Long(0),
    });
    ifd0_entries.sort_by_key(|entry| entry.tag);
    let ifd0_offset = 8u32;
    let exif_ifd_offset = ifd0_offset.saturating_add(encoded_ifd_block_len(&ifd0_entries));
    if let Some(pointer) = ifd0_entries.iter_mut().find(|entry| entry.tag == 0x8769) {
        pointer.value = ExifValue::Long(exif_ifd_offset);
    }

    let ifd0 = encode_ifd_block(&ifd0_entries, ifd0_offset);
    let exif_ifd = encode_ifd_block(&exif_entries, exif_ifd_offset);
    let mut output = Vec::with_capacity(8 + ifd0.len() + exif_ifd.len());
    output.extend_from_slice(b"II");
    output.extend_from_slice(&42u16.to_le_bytes());
    output.extend_from_slice(&ifd0_offset.to_le_bytes());
    output.extend_from_slice(&ifd0);
    output.extend_from_slice(&exif_ifd);
    output
}
