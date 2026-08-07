use super::*;

const UNIQUE_CAMERA_MODEL: u16 = 50708;
const COLOR_MATRIX_1: u16 = 50721;
const COLOR_MATRIX_2: u16 = 50722;
const CAMERA_CALIBRATION_1: u16 = 50723;
const CAMERA_CALIBRATION_2: u16 = 50724;
const CALIBRATION_ILLUMINANT_1: u16 = 50778;
const CALIBRATION_ILLUMINANT_2: u16 = 50779;
const CAMERA_CALIBRATION_SIGNATURE: u16 = 50931;
const PROFILE_CALIBRATION_SIGNATURE: u16 = 50932;
const PROFILE_NAME: u16 = 50936;
const PROFILE_HUE_SAT_MAP_DIMS: u16 = 50937;
const PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
const PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50939;
const PROFILE_TONE_CURVE: u16 = 50940;
const FORWARD_MATRIX_1: u16 = 50964;
const FORWARD_MATRIX_2: u16 = 50965;
const PROFILE_LOOK_TABLE_DIMS: u16 = 50981;
const PROFILE_LOOK_TABLE_DATA: u16 = 50982;
const PROFILE_HUE_SAT_MAP_ENCODING: u16 = 51107;
const PROFILE_LOOK_TABLE_ENCODING: u16 = 51108;
const BASELINE_EXPOSURE_OFFSET: u16 = 51109;

fn has_profile_tags(tags: &[IfdEntry]) -> bool {
    tags.iter().any(|tag| {
        matches!(
            tag.tag,
            PROFILE_HUE_SAT_MAP_DATA_1
                | PROFILE_HUE_SAT_MAP_DATA_2
                | PROFILE_LOOK_TABLE_DATA
                | PROFILE_TONE_CURVE
                | PROFILE_NAME
                | COLOR_MATRIX_1
                | COLOR_MATRIX_2
                | FORWARD_MATRIX_1
                | FORWARD_MATRIX_2
        )
    })
}

pub(super) fn profile_identity_from_tags(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
) -> Result<Option<(Option<String>, Option<String>)>> {
    if !has_profile_tags(tags) {
        return Ok(None);
    }
    Ok(Some((
        read_ascii_tag(reader, tags, PROFILE_NAME)?,
        read_ascii_tag(reader, tags, UNIQUE_CAMERA_MODEL)?,
    )))
}

pub(super) fn profile_from_tags(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
) -> Result<Option<DcpProfile>> {
    if !has_profile_tags(tags) {
        return Ok(None);
    }

    let name = read_ascii_tag(reader, tags, PROFILE_NAME)?;
    let camera_model = read_ascii_tag(reader, tags, UNIQUE_CAMERA_MODEL)?;
    let camera_calibration_signature = read_ascii_tag(reader, tags, CAMERA_CALIBRATION_SIGNATURE)?;
    let calibration_signature = read_ascii_tag(reader, tags, PROFILE_CALIBRATION_SIGNATURE)?;
    let hue_dims = read_u32_tag(reader, tags, PROFILE_HUE_SAT_MAP_DIMS)?;
    let hue_dims = (hue_dims.len() >= 3).then(|| [hue_dims[0], hue_dims[1], hue_dims[2]]);
    let hue_encoding = ProfileEncoding::from_tag(
        read_u32_tag(reader, tags, PROFILE_HUE_SAT_MAP_ENCODING)?
            .first()
            .copied(),
    )?;
    let hue_sat_maps = [
        read_hsv_map(
            reader,
            tags,
            PROFILE_HUE_SAT_MAP_DATA_1,
            hue_dims,
            hue_encoding,
        )?,
        read_hsv_map(
            reader,
            tags,
            PROFILE_HUE_SAT_MAP_DATA_2,
            hue_dims,
            hue_encoding,
        )?,
    ];

    let look_dims = read_u32_tag(reader, tags, PROFILE_LOOK_TABLE_DIMS)?;
    let look_dims = (look_dims.len() >= 3).then(|| [look_dims[0], look_dims[1], look_dims[2]]);
    let look_encoding = ProfileEncoding::from_tag(
        read_u32_tag(reader, tags, PROFILE_LOOK_TABLE_ENCODING)?
            .first()
            .copied(),
    )?;
    let look_table = read_hsv_map(
        reader,
        tags,
        PROFILE_LOOK_TABLE_DATA,
        look_dims,
        look_encoding,
    )?;

    let tone_values = read_f32_tag(reader, tags, PROFILE_TONE_CURVE)?;
    let tone_curve = if tone_values.is_empty() {
        None
    } else {
        if tone_values.len() % 2 != 0 {
            bail!("ProfileToneCurve contains an odd number of values");
        }
        let point_count = tone_values.len() / 2;
        if point_count > MAX_DCP_TONE_POINTS {
            bail!(
                "ProfileToneCurve contains {point_count} points; the safe limit is {MAX_DCP_TONE_POINTS}"
            );
        }
        let mut points = Vec::new();
        points
            .try_reserve_exact(point_count)
            .context("reserve DCP tone curve")?;
        points.extend(tone_values.chunks_exact(2).map(|pair| [pair[0], pair[1]]));
        Some(ToneCurve::new(points)?)
    };

    let matrices = [
        DcpMatrixSet {
            illuminant: read_u32_tag(reader, tags, CALIBRATION_ILLUMINANT_1)?
                .first()
                .copied()
                .and_then(|v| u16::try_from(v).ok()),
            color_matrix: read_matrix_4x3(reader, tags, COLOR_MATRIX_1)?,
            camera_calibration: read_matrix_4x4(reader, tags, CAMERA_CALIBRATION_1)?,
            forward_matrix: read_matrix_3x4(reader, tags, FORWARD_MATRIX_1)?,
        },
        DcpMatrixSet {
            illuminant: read_u32_tag(reader, tags, CALIBRATION_ILLUMINANT_2)?
                .first()
                .copied()
                .and_then(|v| u16::try_from(v).ok()),
            color_matrix: read_matrix_4x3(reader, tags, COLOR_MATRIX_2)?,
            camera_calibration: read_matrix_4x4(reader, tags, CAMERA_CALIBRATION_2)?,
            forward_matrix: read_matrix_3x4(reader, tags, FORWARD_MATRIX_2)?,
        },
    ];
    let baseline_values = read_f32_tag(reader, tags, BASELINE_EXPOSURE_OFFSET)?;
    let baseline_exposure_offset = parse_baseline_exposure_offset(&baseline_values)?;

    Ok(Some(DcpProfile {
        name,
        camera_model,
        camera_calibration_signature,
        calibration_signature,
        matrices,
        hue_sat_maps,
        look_table,
        tone_curve,
        baseline_exposure_offset,
    }))
}

fn parse_baseline_exposure_offset(values: &[f32]) -> Result<f32> {
    let value = values.first().copied().unwrap_or(0.0);
    if !value.is_finite() {
        bail!("BaselineExposureOffset contains a non-finite value");
    }
    Ok(value)
}

fn read_hsv_map(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
    tag: u16,
    dimensions: Option<[u32; 3]>,
    encoding: ProfileEncoding,
) -> Result<Option<HsvMap>> {
    let values = read_f32_tag(reader, tags, tag)?;
    if values.is_empty() {
        return Ok(None);
    }
    let dimensions = dimensions.ok_or_else(|| anyhow!("DCP map {tag} has no dimensions tag"))?;
    let expected = checked_map_len(dimensions)?
        .checked_mul(3)
        .ok_or_else(|| anyhow!("DCP map value count overflow"))?;
    if values.len() != expected {
        bail!(
            "DCP map {tag} has {} scalar values, expected {expected}",
            values.len()
        );
    }
    let entry_count = expected / 3;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(entry_count)
        .context("reserve DCP HSV map")?;
    entries.extend(
        values
            .chunks_exact(3)
            .map(|entry| [entry[0], entry[1], entry[2]]),
    );
    Ok(Some(HsvMap::new(dimensions, entries, encoding)?))
}

fn read_matrix_4x3(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
    tag: u16,
) -> Result<Option<[[f32; 3]; 4]>> {
    let values = read_f32_tag(reader, tags, tag)?;
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 9 && values.len() != 12 {
        bail!(
            "DCP matrix tag {tag} has {} values, expected 9 or 12",
            values.len()
        );
    }
    let mut out = [[0.0; 3]; 4];
    for row in 0..(values.len() / 3) {
        out[row].copy_from_slice(&values[row * 3..row * 3 + 3]);
    }
    Ok(Some(out))
}

fn read_matrix_3x4(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
    tag: u16,
) -> Result<Option<[[f32; 4]; 3]>> {
    let values = read_f32_tag(reader, tags, tag)?;
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 9 && values.len() != 12 {
        bail!(
            "DCP matrix tag {tag} has {} values, expected 9 or 12",
            values.len()
        );
    }
    let planes = values.len() / 3;
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        out[row][..planes].copy_from_slice(&values[row * planes..row * planes + planes]);
    }
    Ok(Some(out))
}

fn read_matrix_4x4(
    reader: &mut TiffReader,
    tags: &[IfdEntry],
    tag: u16,
) -> Result<Option<[[f32; 4]; 4]>> {
    let values = read_f32_tag(reader, tags, tag)?;
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 9 && values.len() != 16 {
        bail!(
            "DCP matrix tag {tag} has {} values, expected 9 or 16",
            values.len()
        );
    }
    let planes = if values.len() == 9 { 3 } else { 4 };
    let mut out = [[0.0; 4]; 4];
    for row in 0..planes {
        out[row][..planes].copy_from_slice(&values[row * planes..row * planes + planes]);
    }
    // Preserve an unused fourth plane when a normal three-channel profile is
    // expanded into the fixed-size internal representation.
    if planes == 3 {
        out[3][3] = 1.0;
    }
    Ok(Some(out))
}

fn find_tag(tags: &[IfdEntry], tag: u16) -> Option<&IfdEntry> {
    tags.iter().find(|entry| entry.tag == tag)
}

fn read_ascii_tag(reader: &mut TiffReader, tags: &[IfdEntry], tag: u16) -> Result<Option<String>> {
    let Some(entry) = find_tag(tags, tag) else {
        return Ok(None);
    };
    let bytes = reader.entry_bytes(entry)?;
    let text = String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .trim()
        .to_owned();
    Ok((!text.is_empty()).then_some(text))
}

fn read_u32_tag(reader: &mut TiffReader, tags: &[IfdEntry], tag: u16) -> Result<Vec<u32>> {
    let Some(entry) = find_tag(tags, tag) else {
        return Ok(Vec::new());
    };
    reader.entry_u32(entry)
}

fn read_f32_tag(reader: &mut TiffReader, tags: &[IfdEntry], tag: u16) -> Result<Vec<f32>> {
    let Some(entry) = find_tag(tags, tag) else {
        return Ok(Vec::new());
    };
    reader.entry_f32(entry)
}

#[derive(Clone, Copy, Debug)]
enum Endian {
    Little,
    Big,
}

impl Endian {
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
    fn i32(self, bytes: [u8; 4]) -> i32 {
        match self {
            Self::Little => i32::from_le_bytes(bytes),
            Self::Big => i32::from_be_bytes(bytes),
        }
    }
    fn u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }
    fn f32(self, bytes: [u8; 4]) -> f32 {
        f32::from_bits(self.u32(bytes))
    }
    fn f64(self, bytes: [u8; 8]) -> f64 {
        f64::from_bits(self.u64(bytes))
    }
}

#[derive(Clone, Debug)]
pub(super) struct IfdEntry {
    tag: u16,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
    inline: [u8; 8],
}

pub(super) struct TiffReader {
    file: File,
    endian: Endian,
    big_tiff: bool,
    first_ifd: u64,
    file_len: u64,
}

impl TiffReader {
    pub(super) fn new(mut file: File) -> Result<Self> {
        let file_len = file.metadata()?.len();
        let mut byte_order = [0; 2];
        file.read_exact(&mut byte_order)?;
        let endian = match &byte_order {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => bail!("not a TIFF/DCP byte-order signature"),
        };
        let magic = read_u16(&mut file, endian)?;
        let (big_tiff, first_ifd) = match magic {
            42 | 0x4352 => (false, read_u32(&mut file, endian)? as u64),
            43 => {
                let offset_size = read_u16(&mut file, endian)?;
                let reserved = read_u16(&mut file, endian)?;
                if offset_size != 8 || reserved != 0 {
                    bail!("unsupported BigTIFF offset format");
                }
                (true, read_u64(&mut file, endian)?)
            }
            _ => bail!("not a TIFF, BigTIFF, or standalone DCP file"),
        };
        Ok(Self {
            file,
            endian,
            big_tiff,
            first_ifd,
            file_len,
        })
    }

    pub(super) fn read_primary_ifd(&mut self) -> Result<Vec<IfdEntry>> {
        if self.first_ifd == 0 || self.first_ifd >= self.file_len {
            bail!("invalid TIFF first-IFD offset {}", self.first_ifd);
        }
        self.file.seek(SeekFrom::Start(self.first_ifd))?;
        let count = if self.big_tiff {
            read_u64(&mut self.file, self.endian)?
        } else {
            read_u16(&mut self.file, self.endian)? as u64
        };
        if count > 65_536 {
            bail!("unreasonable TIFF IFD entry count {count}");
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count as usize)
            .context("reserve TIFF IFD entries")?;
        for _ in 0..count {
            let tag = read_u16(&mut self.file, self.endian)?;
            let field_type = read_u16(&mut self.file, self.endian)?;
            if self.big_tiff {
                let count = read_u64(&mut self.file, self.endian)?;
                let mut inline = [0; 8];
                self.file.read_exact(&mut inline)?;
                entries.push(IfdEntry {
                    tag,
                    field_type,
                    count,
                    value_or_offset: self.endian.u64(inline),
                    inline,
                });
            } else {
                let count = read_u32(&mut self.file, self.endian)? as u64;
                let mut small = [0; 4];
                self.file.read_exact(&mut small)?;
                let mut inline = [0; 8];
                inline[..4].copy_from_slice(&small);
                entries.push(IfdEntry {
                    tag,
                    field_type,
                    count,
                    value_or_offset: self.endian.u32(small) as u64,
                    inline,
                });
            }
        }
        Ok(entries)
    }

    fn entry_bytes(&mut self, entry: &IfdEntry) -> Result<Vec<u8>> {
        let type_size = tiff_type_size(entry.field_type)
            .ok_or_else(|| anyhow!("unsupported TIFF field type {}", entry.field_type))?;
        let byte_len = entry
            .count
            .checked_mul(type_size)
            .ok_or_else(|| anyhow!("TIFF field size overflow"))?;
        if byte_len > MAX_DCP_TAG_BYTES {
            bail!(
                "refusing to allocate oversized TIFF profile tag ({byte_len} bytes; limit {MAX_DCP_TAG_BYTES})"
            );
        }
        let inline_size = if self.big_tiff { 8 } else { 4 };
        if byte_len as usize <= inline_size {
            return Ok(entry.inline[..byte_len as usize].to_vec());
        }
        let end = entry
            .value_or_offset
            .checked_add(byte_len)
            .ok_or_else(|| anyhow!("TIFF tag offset overflow"))?;
        if end > self.file_len {
            bail!("TIFF tag {} points outside the file", entry.tag);
        }
        let return_pos = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(entry.value_or_offset))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_len as usize)
            .context("reserve TIFF profile tag")?;
        bytes.resize(byte_len as usize, 0);
        self.file.read_exact(&mut bytes)?;
        self.file.seek(SeekFrom::Start(return_pos))?;
        Ok(bytes)
    }

    fn entry_u32(&mut self, entry: &IfdEntry) -> Result<Vec<u32>> {
        let bytes = self.entry_bytes(entry)?;
        let stride = match entry.field_type {
            1 | 6 | 7 => 1,
            3 => 2,
            4 | 13 => 4,
            16 | 18 => 8,
            _ => bail!("TIFF tag {} is not an integer field", entry.tag),
        };
        anyhow::ensure!(
            bytes.len() % stride == 0,
            "TIFF integer tag {} has a truncated value",
            entry.tag
        );
        let mut values = Vec::new();
        values
            .try_reserve_exact(bytes.len() / stride)
            .context("reserve TIFF integer values")?;
        for chunk in bytes.chunks_exact(stride) {
            let value = match entry.field_type {
                1 | 6 | 7 => u32::from(chunk[0]),
                3 => self.endian.u16([chunk[0], chunk[1]]) as u32,
                4 | 13 => self.endian.u32(checked_array(chunk, "TIFF u32")?),
                16 | 18 => u32::try_from(self.endian.u64(checked_array(chunk, "TIFF u64")?))
                    .context("TIFF integer does not fit in u32")?,
                _ => unreachable!(),
            };
            values.push(value);
        }
        Ok(values)
    }

    fn entry_f32(&mut self, entry: &IfdEntry) -> Result<Vec<f32>> {
        let bytes = self.entry_bytes(entry)?;
        let stride = match entry.field_type {
            3 => 2,
            4 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => bail!("TIFF tag {} is not a numeric profile field", entry.tag),
        };
        anyhow::ensure!(
            bytes.len() % stride == 0,
            "TIFF numeric tag {} has a truncated value",
            entry.tag
        );
        let mut values = Vec::new();
        values
            .try_reserve_exact(bytes.len() / stride)
            .context("reserve TIFF numeric values")?;
        for chunk in bytes.chunks_exact(stride) {
            let value = match entry.field_type {
                3 => self.endian.u16([chunk[0], chunk[1]]) as f32,
                4 => self.endian.u32(checked_array(chunk, "TIFF u32")?) as f32,
                5 => {
                    let numerator = self
                        .endian
                        .u32(checked_array(&chunk[0..4], "TIFF rational numerator")?);
                    let denominator = self
                        .endian
                        .u32(checked_array(&chunk[4..8], "TIFF rational denominator")?);
                    if denominator == 0 {
                        f32::NAN
                    } else {
                        numerator as f32 / denominator as f32
                    }
                }
                10 => {
                    let numerator = self.endian.i32(checked_array(
                        &chunk[0..4],
                        "TIFF signed rational numerator",
                    )?);
                    let denominator = self.endian.i32(checked_array(
                        &chunk[4..8],
                        "TIFF signed rational denominator",
                    )?);
                    if denominator == 0 {
                        f32::NAN
                    } else {
                        numerator as f32 / denominator as f32
                    }
                }
                11 => self.endian.f32(checked_array(chunk, "TIFF f32")?),
                12 => self.endian.f64(checked_array(chunk, "TIFF f64")?) as f32,
                _ => unreachable!(),
            };
            if !value.is_finite() {
                bail!("TIFF profile tag {} contains non-finite values", entry.tag);
            }
            values.push(value);
        }
        Ok(values)
    }
}

fn tiff_type_size(field_type: u16) -> Option<u64> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn read_u16(reader: &mut File, endian: Endian) -> Result<u16> {
    let mut bytes = [0; 2];
    reader.read_exact(&mut bytes)?;
    Ok(endian.u16(bytes))
}
fn read_u32(reader: &mut File, endian: Endian) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(endian.u32(bytes))
}
fn read_u64(reader: &mut File, endian: Endian) -> Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(endian.u64(bytes))
}

fn checked_array<const N: usize>(bytes: &[u8], label: &str) -> Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("{label} requires exactly {N} bytes, got {}", bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::parse_baseline_exposure_offset;

    #[test]
    fn baseline_exposure_offset_rejects_non_finite_values() {
        assert_eq!(parse_baseline_exposure_offset(&[]).unwrap(), 0.0);
        assert_eq!(parse_baseline_exposure_offset(&[0.25]).unwrap(), 0.25);
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(parse_baseline_exposure_offset(&[value]).is_err());
        }
    }
}
