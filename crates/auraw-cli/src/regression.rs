//! Deterministic writers used by the image-quality regression binary.

use anyhow::{anyhow, Context, Result};
use std::fs::{self, File};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

/// Writes the canonical AuRaw scene-linear NPZ container without compression.
/// The archive and NPY members use fixed headers/timestamps so identical input
/// pixels and metadata produce identical bytes across machines.
pub fn write_linear_rgb_npz(
    path: &Path,
    width: u32,
    height: u32,
    rgb: &[f32],
    metadata_json: &str,
) -> Result<()> {
    let expected = width as usize * height as usize * 3;
    if rgb.len() != expected {
        return Err(anyhow!(
            "RGB buffer contains {} floats, expected {} for {}x{}x3",
            rgb.len(),
            expected,
            width,
            height
        ));
    }
    if rgb.iter().any(|value| !value.is_finite()) {
        return Err(anyhow!("RGB buffer contains NaN or infinity"));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file =
            File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
        let mut archive = StoredZipWriter::new(&mut file);
        archive.add("rgb.npy", &rgb_npy(width, height, rgb)?)?;
        archive.add("metadata_json.npy", &bytes_npy(metadata_json.as_bytes())?)?;
        archive.finish()?;
        file.sync_all()
            .with_context(|| format!("flush {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_else(|| "auraw-regression.npz".into());
    name.push(".tmp");
    path.with_file_name(name)
}

fn rgb_npy(width: u32, height: u32, rgb: &[f32]) -> Result<Vec<u8>> {
    let header = npy_header("<f4", &[height as usize, width as usize, 3])?;
    let mut bytes = Vec::with_capacity(header.len() + rgb.len() * 4);
    bytes.extend_from_slice(&header);
    for value in rgb {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

fn bytes_npy(value: &[u8]) -> Result<Vec<u8>> {
    let header = npy_header("|u1", &[value.len()])?;
    let mut bytes = Vec::with_capacity(header.len() + value.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(value);
    Ok(bytes)
}

fn npy_header(descr: &str, shape: &[usize]) -> Result<Vec<u8>> {
    let shape_text = match shape {
        [only] => format!("({only},)"),
        _ => format!(
            "({})",
            shape
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let mut dictionary =
        format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape_text}, }}")
            .into_bytes();
    // NPY v1 aligns the magic, version, length, and header to 16 bytes.
    let prefix_len = 6 + 2 + 2;
    let padding = (16 - ((prefix_len + dictionary.len() + 1) % 16)) % 16;
    dictionary.extend(std::iter::repeat_n(b' ', padding));
    dictionary.push(b'\n');
    let header_len = u16::try_from(dictionary.len())
        .map_err(|_| anyhow!("NPY header is too large for version 1.0"))?;

    let mut output = Vec::with_capacity(prefix_len + dictionary.len());
    output.extend_from_slice(b"\x93NUMPY");
    output.extend_from_slice(&[1, 0]);
    output.extend_from_slice(&header_len.to_le_bytes());
    output.extend_from_slice(&dictionary);
    Ok(output)
}

#[derive(Clone)]
struct ZipEntry {
    name: Vec<u8>,
    crc32: u32,
    size: u32,
    local_offset: u32,
}

struct StoredZipWriter<'a, W: Write + Seek> {
    writer: &'a mut W,
    entries: Vec<ZipEntry>,
}

impl<'a, W: Write + Seek> StoredZipWriter<'a, W> {
    fn new(writer: &'a mut W) -> Self {
        Self {
            writer,
            entries: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, payload: &[u8]) -> Result<()> {
        let name = name.as_bytes().to_vec();
        let name_len =
            u16::try_from(name.len()).map_err(|_| anyhow!("ZIP member name too long"))?;
        let size = u32::try_from(payload.len()).map_err(|_| anyhow!("ZIP member exceeds 4 GiB"))?;
        let local_offset = u32::try_from(self.writer.stream_position()?)
            .map_err(|_| anyhow!("ZIP archive exceeds 4 GiB"))?;
        let crc32 = crc32(payload);

        write_u32(self.writer, 0x0403_4b50)?;
        write_u16(self.writer, 20)?;
        write_u16(self.writer, 0)?;
        write_u16(self.writer, 0)?; // stored
        write_u16(self.writer, 0)?; // fixed 1980-01-01 00:00
        write_u16(self.writer, 0x0021)?;
        write_u32(self.writer, crc32)?;
        write_u32(self.writer, size)?;
        write_u32(self.writer, size)?;
        write_u16(self.writer, name_len)?;
        write_u16(self.writer, 0)?;
        self.writer.write_all(&name)?;
        self.writer.write_all(payload)?;
        self.entries.push(ZipEntry {
            name,
            crc32,
            size,
            local_offset,
        });
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let central_offset = self.writer.stream_position()?;
        for entry in &self.entries {
            let name_len =
                u16::try_from(entry.name.len()).map_err(|_| anyhow!("ZIP member name too long"))?;
            write_u32(self.writer, 0x0201_4b50)?;
            write_u16(self.writer, 20)?;
            write_u16(self.writer, 20)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0x0021)?;
            write_u32(self.writer, entry.crc32)?;
            write_u32(self.writer, entry.size)?;
            write_u32(self.writer, entry.size)?;
            write_u16(self.writer, name_len)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0)?;
            write_u16(self.writer, 0)?;
            write_u32(self.writer, 0)?;
            write_u32(self.writer, entry.local_offset)?;
            self.writer.write_all(&entry.name)?;
        }
        let central_end = self.writer.stream_position()?;
        let central_size = u32::try_from(central_end - central_offset)
            .map_err(|_| anyhow!("ZIP central directory exceeds 4 GiB"))?;
        let central_offset =
            u32::try_from(central_offset).map_err(|_| anyhow!("ZIP archive exceeds 4 GiB"))?;
        let count = u16::try_from(self.entries.len())
            .map_err(|_| anyhow!("ZIP archive contains too many members"))?;
        write_u32(self.writer, 0x0605_4b50)?;
        write_u16(self.writer, 0)?;
        write_u16(self.writer, 0)?;
        write_u16(self.writer, count)?;
        write_u16(self.writer, count)?;
        write_u32(self.writer, central_size)?;
        write_u32(self.writer, central_offset)?;
        write_u16(self.writer, 0)?;
        self.writer.flush()?;
        Ok(())
    }
}

fn write_u16(writer: &mut impl Write, value: u16) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_u32(writer: &mut impl Write, value: u32) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{crc32, npy_header};

    #[test]
    fn crc_matches_zip_reference_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn npy_headers_are_aligned_and_newline_terminated() {
        for header in [
            npy_header("<f4", &[12, 16, 3]).unwrap(),
            npy_header("|u1", &[2]).unwrap(),
        ] {
            assert_eq!(header.len() % 16, 0);
            let header_len = u16::from_le_bytes([header[8], header[9]]) as usize;
            assert_eq!(header_len + 10, header.len());
            assert_eq!(header.last(), Some(&b'\n'));
        }
    }
}
