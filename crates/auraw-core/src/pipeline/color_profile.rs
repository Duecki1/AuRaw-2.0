use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

mod dcp;
mod icc;

use crate::color_math::{rec2020_from_oklab, rec2020_to_oklab};
use dcp::{profile_from_tags, profile_identity_from_tags, TiffReader};
#[cfg(target_os = "android")]
use icc::MatrixShaperProfile;

#[cfg(test)]
mod tests;

pub const OUTPUT_LUT_EDGE: u32 = 33;
const PROFILE_TONE_LUT_SIZE: usize = 4096;
const MAX_DCP_TAG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DCP_MAP_ENTRIES: usize = 1_000_000;
const MAX_DCP_TONE_POINTS: usize = 65_536;
#[cfg(any(target_os = "android", test))]
const D50_XYZ: [f32; 3] = [0.964_22, 1.0, 0.825_21];

pub(super) fn convert_embedded_icc_rgb_to_rec2020(bytes: &[u8], rgb: &mut [f32]) -> Result<()> {
    icc::convert_input_rgb_to_rec2020(bytes, rgb)
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug)]
pub struct DisplayIccProfile {
    pub bytes: Vec<u8>,
    pub label: String,
    pub source: String,
}

#[cfg(not(target_os = "android"))]
pub fn read_display_icc_profile(path: &Path) -> Result<DisplayIccProfile> {
    let profile = icc::read_display_profile_file(path)?;
    Ok(DisplayIccProfile {
        bytes: profile.bytes,
        label: profile.label,
        source: profile.source,
    })
}

#[cfg(not(target_os = "android"))]
pub fn discover_display_icc_profile(
    screen_point: Option<[i32; 2]>,
) -> Result<Option<DisplayIccProfile>> {
    Ok(
        icc::discover_display_profile(screen_point)?.map(|profile| DisplayIccProfile {
            bytes: profile.bytes,
            label: profile.label,
            source: profile.source,
        }),
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileEncoding {
    #[default]
    Linear,
    Srgb,
}

impl ProfileEncoding {
    fn from_tag(value: Option<u32>) -> Result<Self> {
        match value {
            None | Some(0) => Ok(Self::Linear),
            Some(1) => Ok(Self::Srgb),
            Some(other) => bail!("unsupported DCP profile-table encoding {other}"),
        }
    }

    pub fn shader_value(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Srgb => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HsvMap {
    pub divisions: [u32; 3],
    pub entries: Vec<[f32; 3]>,
    pub encoding: ProfileEncoding,
}

impl HsvMap {
    pub fn new(
        divisions: [u32; 3],
        entries: Vec<[f32; 3]>,
        encoding: ProfileEncoding,
    ) -> Result<Self> {
        let expected = checked_map_len(divisions)?;
        if entries.len() != expected {
            bail!(
                "DCP HSV map contains {} entries, expected {} for {:?}",
                entries.len(),
                expected,
                divisions
            );
        }
        if divisions[0] < 1 || divisions[1] < 2 || divisions[2] < 1 {
            bail!("invalid DCP HSV map dimensions {divisions:?}");
        }
        if entries.iter().flatten().any(|v| !v.is_finite()) {
            bail!("DCP HSV map contains a non-finite value");
        }
        let [hue_count, saturation_count, value_count] = divisions;
        for value in 0..value_count {
            for hue in 0..hue_count {
                let index = ((value * hue_count + hue) * saturation_count) as usize;
                if (entries[index][2] - 1.0).abs() > 1e-5 {
                    bail!(
                        "DCP HSV map zero-saturation entry has value scale {}, expected 1",
                        entries[index][2]
                    );
                }
            }
        }
        Ok(Self {
            divisions,
            entries,
            encoding,
        })
    }

    fn interpolate(a: &Self, b: &Self, weight: f32) -> Option<Self> {
        if a.divisions != b.divisions || a.encoding != b.encoding {
            return None;
        }
        let t = weight.clamp(0.0, 1.0);
        let entries = a
            .entries
            .iter()
            .zip(&b.entries)
            .map(|(left, right)| {
                [
                    left[0] + (right[0] - left[0]) * t,
                    left[1] + (right[1] - left[1]) * t,
                    left[2] + (right[2] - left[2]) * t,
                ]
            })
            .collect();
        Some(Self {
            divisions: a.divisions,
            entries,
            encoding: a.encoding,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToneCurve {
    pub points: Vec<[f32; 2]>,
}

impl ToneCurve {
    pub fn new(points: Vec<[f32; 2]>) -> Result<Self> {
        if points.iter().flatten().any(|v| !v.is_finite()) {
            bail!("DCP tone curve contains a non-finite value");
        }
        if points.len() < 2 {
            bail!("DCP tone curve needs at least two points");
        }
        if points
            .iter()
            .any(|point| !(0.0..=1.0).contains(&point[0]) || !(0.0..=1.0).contains(&point[1]))
        {
            bail!("DCP tone-curve coordinates must be in the 0..1 range");
        }
        if points.windows(2).any(|pair| pair[1][0] <= pair[0][0]) {
            bail!("DCP tone-curve x coordinates must be stored in strictly increasing order");
        }
        Ok(Self { points })
    }

    pub fn sampled_lut(&self, size: usize) -> Vec<f32> {
        let size = size.max(2);
        let points = normalized_curve_points(&self.points);
        let second_derivatives = natural_cubic_second_derivatives(&points);
        (0..size)
            .map(|index| {
                let x = index as f32 / (size - 1) as f32;
                sample_natural_cubic(&points, &second_derivatives, x)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DcpMatrixSet {
    pub illuminant: Option<u16>,
    pub color_matrix: Option<[[f32; 3]; 4]>,
    pub camera_calibration: Option<[[f32; 4]; 4]>,
    pub forward_matrix: Option<[[f32; 4]; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct DcpProfileIdentity {
    pub name: Option<String>,
    pub camera_model: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DcpProfile {
    pub name: Option<String>,
    pub camera_model: Option<String>,
    pub camera_calibration_signature: Option<String>,
    pub calibration_signature: Option<String>,
    pub matrices: [DcpMatrixSet; 2],
    pub hue_sat_maps: [Option<HsvMap>; 2],
    pub look_table: Option<HsvMap>,
    pub tone_curve: Option<ToneCurve>,
    pub baseline_exposure_offset: f32,
}

impl DcpProfile {
    pub fn identity_from_path(path: &Path) -> Result<Option<DcpProfileIdentity>> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut signature = [0u8; 4];
        if file.read_exact(&mut signature).is_err() {
            return Ok(None);
        }
        let classic_tiff = signature == *b"II*\0" || signature == *b"MM\0*";
        let big_tiff = signature == *b"II+\0" || signature == *b"MM\0+";
        let standalone_dcp = signature == *b"IIRC" || signature == *b"MMCR";
        if !classic_tiff && !big_tiff && !standalone_dcp {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut tiff = TiffReader::new(file)?;
        let tags = tiff.read_primary_ifd()?;
        let Some((name, camera_model)) = profile_identity_from_tags(&mut tiff, &tags)? else {
            return Ok(None);
        };
        Ok(Some(DcpProfileIdentity { name, camera_model }))
    }

    pub fn from_path(path: &Path) -> Result<Option<Self>> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut signature = [0u8; 4];
        if file.read_exact(&mut signature).is_err() {
            return Ok(None);
        }
        let classic_tiff = signature == *b"II*\0" || signature == *b"MM\0*";
        let big_tiff = signature == *b"II+\0" || signature == *b"MM\0+";
        let standalone_dcp = signature == *b"IIRC" || signature == *b"MMCR";
        if !classic_tiff && !big_tiff && !standalone_dcp {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut tiff = TiffReader::new(file)?;
        let tags = tiff.read_primary_ifd()?;
        let Some(profile) = profile_from_tags(&mut tiff, &tags)? else {
            return Ok(None);
        };
        Ok(Some(profile))
    }

    pub fn calibration_is_compatible(&self) -> bool {
        self.camera_calibration_signature
            .as_deref()
            .unwrap_or_default()
            == self.calibration_signature.as_deref().unwrap_or_default()
    }

    pub fn interpolated_hue_sat_map(&self, weight: f32) -> Option<HsvMap> {
        match (&self.hue_sat_maps[0], &self.hue_sat_maps[1]) {
            (Some(a), Some(b)) => HsvMap::interpolate(a, b, weight)
                .or_else(|| Some(if weight < 0.5 { a.clone() } else { b.clone() })),
            (Some(map), None) | (None, Some(map)) => Some(map.clone()),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CameraProfile {
    pub name: Option<String>,
    pub profile_exposure_offset_ev: f32,
    pub default_exposure_ev: f32,
    pub hue_sat_maps: [Option<HsvMap>; 2],
    pub look_table: Option<HsvMap>,
    pub tone_curve: Option<ToneCurve>,
    pub interpolation_weight: f32,
    pub embedded_camera_icc: Option<Vec<u8>>,
}

impl CameraProfile {
    pub fn from_dcp(dcp: DcpProfile, interpolation_weight: f32) -> Self {
        let maps_are_compatible = match (&dcp.hue_sat_maps[0], &dcp.hue_sat_maps[1]) {
            (Some(first), Some(second)) => {
                first.divisions == second.divisions && first.encoding == second.encoding
            }
            _ => true,
        };
        let fallback_map = (!maps_are_compatible)
            .then(|| dcp.interpolated_hue_sat_map(interpolation_weight))
            .flatten();
        let hue_sat_maps = if maps_are_compatible {
            dcp.hue_sat_maps
        } else {
            [fallback_map, None]
        };
        Self {
            name: dcp.name,
            profile_exposure_offset_ev: dcp.baseline_exposure_offset,
            default_exposure_ev: dcp.baseline_exposure_offset,
            hue_sat_maps,
            look_table: dcp.look_table,
            tone_curve: dcp.tone_curve,
            interpolation_weight: interpolation_weight.clamp(0.0, 1.0),
            embedded_camera_icc: None,
        }
    }

    pub fn gpu_layout(&self) -> ProfileGpuLayout {
        ProfileGpuLayout::new(self)
    }

    pub fn gpu_data(&self, output: &IccOutputTransform) -> ProfileGpuData {
        ProfileGpuData::new(self, output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

#[derive(Clone, Debug)]
pub struct IccOutputTransform {
    size: u32,
    entries: Vec<[f32; 4]>,
}

impl IccOutputTransform {
    pub fn srgb() -> Self {
        let size = OUTPUT_LUT_EDGE;
        let mut entries = Vec::with_capacity((size * size * size) as usize);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let rec2020 = [
                        output_lut_linear_node(r, size),
                        output_lut_linear_node(g, size),
                        output_lut_linear_node(b, size),
                    ];
                    let encoded = display_linear_rec2020_to_srgb(rec2020);
                    entries.push([encoded[0], encoded[1], encoded[2], 0.0]);
                }
            }
        }
        Self { size, entries }
    }

    pub fn from_icc(bytes: &[u8], intent: RenderingIntent) -> Result<Self> {
        let size = OUTPUT_LUT_EDGE;
        #[cfg(not(target_os = "android"))]
        {
            let entries = icc::build_lcms_output_lut(bytes, intent, size)?;
            Ok(Self { size, entries })
        }

        #[cfg(target_os = "android")]
        {
            let profile = MatrixShaperProfile::parse(bytes)?;
            let mut entries = Vec::with_capacity((size * size * size) as usize);
            for b in 0..size {
                for g in 0..size {
                    for r in 0..size {
                        let rgb = [
                            output_lut_linear_node(r, size),
                            output_lut_linear_node(g, size),
                            output_lut_linear_node(b, size),
                        ];
                        let encoded = profile.transform(rgb, intent);
                        entries.push([encoded[0], encoded[1], encoded[2], 0.0]);
                    }
                }
            }
            Ok(Self { size, entries })
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn transform_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        sample_rgb_lut(&self.entries, self.size, rgb)
    }

    pub fn entries(&self) -> &[[f32; 4]] {
        &self.entries
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProfileGpuLayout {
    pub hue_sat: [u32; 4],
    pub hue_sat_2: [u32; 4],
    pub look: [u32; 4],
    pub tone: [u32; 4],
    pub output: [u32; 4],
    pub flags: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct CameraCharacterizationGpuStage {
    pub hue_sat: [u32; 4],
    pub hue_sat_2: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct OptionalLookGpuStage {
    pub look_table: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct ViewTransformGpuStage {
    pub profile_tone: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct OutputEncodingGpuStage {
    pub output_lut: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct ProfileGpuStages {
    pub characterization: CameraCharacterizationGpuStage,
    pub optional_look: OptionalLookGpuStage,
    pub view: ViewTransformGpuStage,
    pub output: OutputEncodingGpuStage,
}

impl ProfileGpuLayout {
    pub fn stages(self) -> ProfileGpuStages {
        ProfileGpuStages {
            characterization: CameraCharacterizationGpuStage {
                hue_sat: self.hue_sat,
                hue_sat_2: self.hue_sat_2,
            },
            optional_look: OptionalLookGpuStage {
                look_table: self.look,
            },
            view: ViewTransformGpuStage {
                profile_tone: self.tone,
            },
            output: OutputEncodingGpuStage {
                output_lut: self.output,
            },
        }
    }

    fn new(profile: &CameraProfile) -> Self {
        let mut offset = 1u32;
        let hue_sat = if let Some(map) = &profile.hue_sat_maps[0] {
            let out = [map.divisions[0], map.divisions[1], map.divisions[2], offset];
            offset += map.entries.len() as u32;
            out
        } else if let Some(map) = &profile.hue_sat_maps[1] {
            let out = [map.divisions[0], map.divisions[1], map.divisions[2], offset];
            offset += map.entries.len() as u32;
            out
        } else {
            [0; 4]
        };
        let hue_sat_2 = if profile.hue_sat_maps[0].is_some() {
            if let Some(map) = &profile.hue_sat_maps[1] {
                let out = [map.divisions[0], map.divisions[1], map.divisions[2], offset];
                offset += map.entries.len() as u32;
                out
            } else {
                [0; 4]
            }
        } else {
            [0; 4]
        };
        let look = if let Some(map) = &profile.look_table {
            let out = [map.divisions[0], map.divisions[1], map.divisions[2], offset];
            offset += map.entries.len() as u32;
            out
        } else {
            [0; 4]
        };
        let tone = if profile.tone_curve.is_some() {
            let out = [PROFILE_TONE_LUT_SIZE as u32, offset, 0, 0];
            offset += PROFILE_TONE_LUT_SIZE as u32;
            out
        } else {
            [0; 4]
        };
        let output = [OUTPUT_LUT_EDGE, OUTPUT_LUT_EDGE, OUTPUT_LUT_EDGE, offset];
        let flags = [
            profile
                .hue_sat_maps
                .iter()
                .flatten()
                .next()
                .map_or(0, |map| map.encoding.shader_value()),
            profile
                .look_table
                .as_ref()
                .map_or(0, |map| map.encoding.shader_value()),
            profile.default_exposure_ev.to_bits(),
            0,
        ];
        Self {
            hue_sat,
            hue_sat_2,
            look,
            tone,
            output,
            flags,
        }
    }
}

pub struct ProfileGpuData {
    pub layout: ProfileGpuLayout,
    pub words: Vec<[f32; 4]>,
}

impl ProfileGpuData {
    fn new(profile: &CameraProfile, output: &IccOutputTransform) -> Self {
        let layout = profile.gpu_layout();
        debug_assert_eq!(output.size(), OUTPUT_LUT_EDGE);
        let total = layout.output[3] as usize + output.entries().len();
        let mut words = Vec::with_capacity(total);
        words.push(layout.hue_sat_2.map(f32::from_bits));
        if let Some(map) = profile.hue_sat_maps[0]
            .as_ref()
            .or(profile.hue_sat_maps[1].as_ref())
        {
            words.extend(
                map.entries
                    .iter()
                    .map(|entry| [entry[0], entry[1], entry[2], 0.0]),
            );
        }
        if profile.hue_sat_maps[0].is_some() {
            if let Some(map) = &profile.hue_sat_maps[1] {
                words.extend(
                    map.entries
                        .iter()
                        .map(|entry| [entry[0], entry[1], entry[2], 0.0]),
                );
            }
        }
        if let Some(map) = &profile.look_table {
            words.extend(
                map.entries
                    .iter()
                    .map(|entry| [entry[0], entry[1], entry[2], 0.0]),
            );
        }
        if let Some(curve) = &profile.tone_curve {
            words.extend(
                curve
                    .sampled_lut(PROFILE_TONE_LUT_SIZE)
                    .into_iter()
                    .map(|value| [value, 0.0, 0.0, 0.0]),
            );
        }
        debug_assert_eq!(words.len(), layout.output[3] as usize);
        words.extend_from_slice(output.entries());
        Self { layout, words }
    }

    pub fn validate(&self) -> Result<()> {
        if self.words.is_empty() {
            bail!("GPU profile buffer must contain its metadata word");
        }

        let mut cursor = 1usize;
        cursor = validate_profile_map_region(
            "primary HueSat map",
            self.layout.hue_sat,
            cursor,
            self.words.len(),
        )?;
        cursor = validate_profile_map_region(
            "secondary HueSat map",
            self.layout.hue_sat_2,
            cursor,
            self.words.len(),
        )?;
        cursor =
            validate_profile_map_region("look table", self.layout.look, cursor, self.words.len())?;

        if self.layout.tone[0] == 0 {
            if self.layout.tone != [0; 4] {
                bail!("disabled profile tone curve has non-zero layout metadata");
            }
        } else {
            let offset = self.layout.tone[1] as usize;
            let count = self.layout.tone[0] as usize;
            if offset != cursor {
                bail!("profile tone curve starts at {offset}, expected {cursor}");
            }
            cursor = cursor
                .checked_add(count)
                .ok_or_else(|| anyhow!("profile tone curve range overflows"))?;
            if cursor > self.words.len() {
                bail!("profile tone curve extends past the packed GPU buffer");
            }
        }

        let output_offset = self.layout.output[3] as usize;
        if output_offset != cursor {
            bail!("output LUT starts at {output_offset}, expected {cursor}");
        }
        let output_entries = checked_map_len([
            self.layout.output[0],
            self.layout.output[1],
            self.layout.output[2],
        ])?;
        let expected_total = output_offset
            .checked_add(output_entries)
            .ok_or_else(|| anyhow!("output LUT range overflows"))?;
        if expected_total != self.words.len() {
            bail!(
                "packed GPU profile contains {} words; layout requires {expected_total}",
                self.words.len()
            );
        }
        Ok(())
    }
}

fn validate_profile_map_region(
    label: &str,
    layout: [u32; 4],
    expected_offset: usize,
    total_words: usize,
) -> Result<usize> {
    if layout[0..3] == [0, 0, 0] {
        if layout[3] != 0 {
            bail!("disabled {label} has a non-zero offset");
        }
        return Ok(expected_offset);
    }
    if layout[0..3].contains(&0) {
        bail!("{label} has a zero dimension");
    }
    let offset = layout[3] as usize;
    if offset != expected_offset {
        bail!("{label} starts at {offset}, expected {expected_offset}");
    }
    let count = checked_map_len([layout[0], layout[1], layout[2]])?;
    let end = offset
        .checked_add(count)
        .ok_or_else(|| anyhow!("{label} range overflows"))?;
    if end > total_words {
        bail!("{label} extends past the packed GPU buffer");
    }
    Ok(end)
}

fn checked_map_len(divisions: [u32; 3]) -> Result<usize> {
    let entries = divisions
        .into_iter()
        .try_fold(1usize, |acc, value| acc.checked_mul(value as usize))
        .ok_or_else(|| anyhow!("DCP HSV map dimensions overflow"))?;
    if entries > MAX_DCP_MAP_ENTRIES {
        bail!("DCP HSV map contains {entries} entries; the safe limit is {MAX_DCP_MAP_ENTRIES}");
    }
    Ok(entries)
}

fn normalized_curve_points(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    points.to_vec()
}

fn natural_cubic_second_derivatives(points: &[[f32; 2]]) -> Vec<f64> {
    let count = points.len();
    let mut second = vec![0.0f64; count];
    if count <= 2 {
        return second;
    }

    let mut rhs = vec![0.0f64; count];
    for index in 1..count - 1 {
        let x_prev = points[index - 1][0] as f64;
        let x = points[index][0] as f64;
        let x_next = points[index + 1][0] as f64;
        let span = x_next - x_prev;
        let sigma = (x - x_prev) / span;
        let pivot = sigma * second[index - 1] + 2.0;
        second[index] = (sigma - 1.0) / pivot;

        let slope_next = (points[index + 1][1] as f64 - points[index][1] as f64) / (x_next - x);
        let slope_prev = (points[index][1] as f64 - points[index - 1][1] as f64) / (x - x_prev);
        rhs[index] = (6.0 * (slope_next - slope_prev) / span - sigma * rhs[index - 1]) / pivot;
    }

    for index in (0..count - 1).rev() {
        second[index] = second[index] * second[index + 1] + rhs[index];
    }
    second
}

fn sample_natural_cubic(points: &[[f32; 2]], second: &[f64], x: f32) -> f32 {
    if x <= points[0][0] {
        return points[0][1];
    }
    let last = points.len() - 1;
    if x >= points[last][0] {
        return points[last][1];
    }

    let upper = points.partition_point(|point| point[0] <= x);
    let lower = upper.saturating_sub(1).min(last - 1);
    let upper = lower + 1;
    let x0 = points[lower][0] as f64;
    let x1 = points[upper][0] as f64;
    let width = x1 - x0;
    let a = (x1 - x as f64) / width;
    let b = (x as f64 - x0) / width;
    let value = a * points[lower][1] as f64
        + b * points[upper][1] as f64
        + ((a * a * a - a) * second[lower] + (b * b * b - b) * second[upper]) * width * width / 6.0;
    value as f32
}

fn mul3(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

#[cfg(any(target_os = "android", test))]
fn invert3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ])
}

pub(super) fn display_linear_rec2020_to_srgb(rgb: [f32; 3]) -> [f32; 3] {
    let linear = mul3(
        [
            [1.660_491, -0.587_641_1, -0.072_849_9],
            [-0.124_550_5, 1.132_899_9, -0.008_349_4],
            [-0.018_150_8, -0.100_578_9, 1.118_729_7],
        ],
        rgb,
    );
    perceptual_gamut_compress(linear).map(srgb_encode)
}

fn perceptual_gamut_compress(rgb: [f32; 3]) -> [f32; 3] {
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    if min >= 0.0 && max <= 1.0 {
        return rgb;
    }
    let luma = (rgb[0] * 0.212_672_9 + rgb[1] * 0.715_152_2 + rgb[2] * 0.072_175).clamp(0.0, 1.0);
    let mut scale: f32 = 1.0;
    for value in rgb {
        let delta = value - luma;
        if delta > 0.0 {
            scale = scale.min((1.0 - luma) / delta);
        } else if delta < 0.0 {
            scale = scale.min((0.0 - luma) / delta);
        }
    }
    rgb.map(|value| (luma + (value - luma) * scale.clamp(0.0, 1.0)).clamp(0.0, 1.0))
}

#[cfg(any(target_os = "android", test))]
fn saturation_gamut_compress(rgb: [f32; 3]) -> [f32; 3] {
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let shifted = rgb.map(|v| v - min.min(0.0));
    let max = shifted[0].max(shifted[1]).max(shifted[2]);
    if max > 1.0 {
        shifted.map(|v| (v / max).clamp(0.0, 1.0))
    } else {
        shifted.map(|v| v.clamp(0.0, 1.0))
    }
}

fn srgb_encode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn output_lut_linear_node(index: u32, size: u32) -> f32 {
    srgb_decode(index as f32 / (size.max(2) - 1) as f32)
}

fn output_lut_shaper(value: f32) -> f32 {
    let magnitude = value.abs();
    if magnitude >= 1.0 {
        return value.signum().clamp(0.0, 1.0);
    }
    let encoded = if magnitude <= 0.003_130_8 {
        magnitude * 12.92
    } else {
        1.055 * magnitude.powf(1.0 / 2.4) - 0.055
    };
    (value.signum() * encoded).clamp(0.0, 1.0)
}

fn rgb_is_unit(rgb: [f32; 3]) -> bool {
    rgb[0].min(rgb[1]).min(rgb[2]) >= -1e-7 && rgb[0].max(rgb[1]).max(rgb[2]) <= 1.000_000_1
}

fn perceptual_soft_chroma(requested: f32, boundary: f32) -> f32 {
    if boundary <= 1e-8 {
        return 0.0;
    }
    let chroma = requested.max(0.0);
    let knee = boundary * 0.90;
    if chroma <= knee {
        return chroma;
    }
    let span = (boundary - knee).max(1e-8);
    (knee + span * (1.0 - (-(chroma - knee) / span).exp())).min(boundary * 0.999_95)
}

fn rec2020_unit_boundary(lightness: f32, hue: [f32; 2], requested: f32) -> f32 {
    let lightness = lightness.clamp(0.0, 1.0);
    let mut low = 0.0;
    let mut high = requested.max(0.04);
    for _ in 0..8 {
        let probe = rec2020_from_oklab([lightness, hue[0] * high, hue[1] * high]);
        if rgb_is_unit(probe) {
            low = high;
            high *= 2.0;
        }
    }
    for _ in 0..11 {
        let middle = 0.5 * (low + high);
        let probe = rec2020_from_oklab([lightness, hue[0] * middle, hue[1] * middle]);
        if rgb_is_unit(probe) {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

fn map_output_lut_input_rec2020(rgb: [f32; 3]) -> [f32; 3] {
    if rgb_is_unit(rgb) {
        return rgb.map(|value| value.clamp(0.0, 1.0));
    }
    let lab = rec2020_to_oklab(rgb);
    let lightness = lab[0].clamp(0.0, 1.0);
    let chroma = lab[1].hypot(lab[2]);
    if chroma <= 1e-9 {
        return rec2020_from_oklab([lightness, 0.0, 0.0]);
    }
    let hue = [lab[1] / chroma, lab[2] / chroma];
    let knee_probe = rec2020_from_oklab([
        lightness,
        hue[0] * (chroma / 0.90),
        hue[1] * (chroma / 0.90),
    ]);
    if (lightness - lab[0]).abs() <= 1e-7 && rgb_is_unit(rgb) && rgb_is_unit(knee_probe) {
        return rgb;
    }
    let boundary = rec2020_unit_boundary(lightness, hue, chroma);
    let compressed = perceptual_soft_chroma(chroma, boundary);
    rec2020_from_oklab([lightness, hue[0] * compressed, hue[1] * compressed])
        .map(|value| value.clamp(0.0, 1.0))
}

fn sample_rgb_lut(entries: &[[f32; 4]], size: u32, rgb: [f32; 3]) -> [f32; 3] {
    let edge = size.max(2);
    let mapped = map_output_lut_input_rec2020(rgb);
    let coord = mapped.map(|value| output_lut_shaper(value) * (edge - 1) as f32);
    let lo = coord.map(|value| (value.floor() as u32).min(edge - 1));
    let hi = lo.map(|value| value.saturating_add(1).min(edge - 1));
    let f = [
        coord[0] - lo[0] as f32,
        coord[1] - lo[1] as f32,
        coord[2] - lo[2] as f32,
    ];
    let fetch = |r: u32, g: u32, b: u32| -> [f32; 3] {
        let index = ((b * edge + g) * edge + r) as usize;
        let entry = entries[index];
        [entry[0], entry[1], entry[2]]
    };
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let c00 = lerp(fetch(lo[0], lo[1], lo[2]), fetch(hi[0], lo[1], lo[2]), f[0]);
    let c10 = lerp(fetch(lo[0], hi[1], lo[2]), fetch(hi[0], hi[1], lo[2]), f[0]);
    let c01 = lerp(fetch(lo[0], lo[1], hi[2]), fetch(hi[0], lo[1], hi[2]), f[0]);
    let c11 = lerp(fetch(lo[0], hi[1], hi[2]), fetch(hi[0], hi[1], hi[2]), f[0]);
    let c0 = lerp(c00, c10, f[1]);
    let c1 = lerp(c01, c11, f[1]);
    lerp(c0, c1, f[2])
}
