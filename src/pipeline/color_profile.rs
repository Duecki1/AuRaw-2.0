use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const OUTPUT_LUT_EDGE: u32 = 33;
const PROFILE_TONE_LUT_SIZE: usize = 4096;
const D50_XYZ: [f32; 3] = [0.964_22, 1.0, 0.825_21];

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

    pub(crate) fn shader_value(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Srgb => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HsvMap {
    /// Hue, saturation, and value divisions.
    pub divisions: [u32; 3],
    /// DNG order: value is outermost, hue is next, saturation is innermost.
    /// Entries contain hue shift in degrees, saturation scale, and value scale.
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
        let first = points[0];
        let last = points[points.len() - 1];
        if first[0].abs() > 1e-6
            || first[1].abs() > 1e-6
            || (last[0] - 1.0).abs() > 1e-6
            || (last[1] - 1.0).abs() > 1e-6
        {
            bail!("SDR DCP tone curves must start at (0, 0) and end at (1, 1)");
        }
        Ok(Self { points })
    }

    /// Samples the DNG profile curve with a natural cubic spline. This is C2
    /// continuous and has zero second derivative at both endpoints, matching
    /// the interpolation model used by Adobe's DNG reference implementation.
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
    /// XYZ to reference-camera matrix (up to four camera planes).
    pub color_matrix: Option<[[f32; 3]; 4]>,
    pub camera_calibration: Option<[[f32; 4]; 4]>,
    /// Reference-camera to XYZ D50 matrix.
    pub forward_matrix: Option<[[f32; 4]; 3]>,
}

impl DcpMatrixSet {
    pub fn interpolate(first: &Self, second: &Self, weight: f32) -> Self {
        let t = weight.clamp(0.0, 1.0);
        Self {
            illuminant: if t < 0.5 {
                first.illuminant
            } else {
                second.illuminant
            },
            color_matrix: interpolate_optional_matrix_4x3(
                first.color_matrix,
                second.color_matrix,
                t,
            ),
            camera_calibration: interpolate_optional_matrix_4x4(
                first.camera_calibration,
                second.camera_calibration,
                t,
            ),
            forward_matrix: interpolate_optional_matrix_3x4(
                first.forward_matrix,
                second.forward_matrix,
                t,
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DcpProfile {
    pub name: Option<String>,
    /// Signature attached to CameraCalibration in IFD 0.
    pub camera_calibration_signature: Option<String>,
    /// Signature attached to the selected camera profile.
    pub calibration_signature: Option<String>,
    pub matrices: [DcpMatrixSet; 2],
    pub hue_sat_maps: [Option<HsvMap>; 2],
    pub look_table: Option<HsvMap>,
    pub tone_curve: Option<ToneCurve>,
    pub baseline_exposure_offset: f32,
}

impl DcpProfile {
    /// Reads profile tags from a DCP or from the first IFD of a DNG/TIFF.
    /// Non-TIFF files return `Ok(None)` so ordinary proprietary RAW loading is
    /// unaffected.
    pub fn from_path(path: &Path) -> Result<Option<Self>> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut signature = [0u8; 4];
        if file.read_exact(&mut signature).is_err() {
            return Ok(None);
        }
        let classic_tiff = signature == *b"II*\0" || signature == *b"MM\0*";
        let big_tiff = signature == *b"II+\0" || signature == *b"MM\0+";
        // Standalone DCP files use TIFF byte ordering followed by the
        // camera-profile magic value 0x4352 ("CR") and a 32-bit IFD offset.
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

    /// CameraCalibration is valid only when both DNG signatures match exactly.
    /// Missing tags have the specified empty-string default, so two missing
    /// signatures are compatible while one missing and one non-empty are not.
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

    pub fn interpolated_matrices(&self, weight: f32) -> DcpMatrixSet {
        DcpMatrixSet::interpolate(&self.matrices[0], &self.matrices[1], weight)
    }

    /// Returns the dual-illuminant blend for a correlated colour temperature.
    /// DNG interpolation is linear in reciprocal temperature (mired space).
    pub fn interpolation_weight_for_cct(&self, cct: f32) -> Option<f32> {
        let first = dng_illuminant_cct(self.matrices[0].illuminant?)?;
        let second = dng_illuminant_cct(self.matrices[1].illuminant?)?;
        let first_mired = 1_000_000.0 / first;
        let second_mired = 1_000_000.0 / second;
        let scene_mired = 1_000_000.0 / cct.max(1.0);
        let denominator = second_mired - first_mired;
        if denominator.abs() < 1e-8 {
            Some(0.0)
        } else {
            Some(((scene_mired - first_mired) / denominator).clamp(0.0, 1.0))
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CameraProfile {
    pub name: Option<String>,
    /// Default exposure contribution in EV. `from_dcp` initializes this
    /// from BaselineExposureOffset; the RAW loader adds DNG BaselineExposure.
    pub baseline_exposure_offset: f32,
    pub hue_sat_map: Option<HsvMap>,
    pub look_table: Option<HsvMap>,
    pub tone_curve: Option<ToneCurve>,
    pub interpolation_weight: f32,
    /// Camera-input ICC bytes copied from LibRaw. They are retained for export
    /// workflows, but DNG matrix/profile processing remains the default path.
    pub embedded_camera_icc: Option<Vec<u8>>,
}

impl CameraProfile {
    pub fn from_dcp(dcp: DcpProfile, interpolation_weight: f32) -> Self {
        // Compute interpolated data before moving owned fields out of `dcp`.
        let hue_sat_map = dcp.interpolated_hue_sat_map(interpolation_weight);
        Self {
            name: dcp.name,
            baseline_exposure_offset: dcp.baseline_exposure_offset,
            hue_sat_map,
            look_table: dcp.look_table,
            tone_curve: dcp.tone_curve,
            interpolation_weight: interpolation_weight.clamp(0.0, 1.0),
            embedded_camera_icc: None,
        }
    }

    pub(crate) fn gpu_layout(&self) -> ProfileGpuLayout {
        ProfileGpuLayout::new(self)
    }

    pub(crate) fn gpu_data(&self, output: &IccOutputTransform) -> ProfileGpuData {
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
                        r as f32 / (size - 1) as f32,
                        g as f32 / (size - 1) as f32,
                        b as f32 / (size - 1) as f32,
                    ];
                    let linear = mul3(
                        [
                            [1.660_491, -0.587_641_1, -0.072_849_9],
                            [-0.124_550_5, 1.132_899_9, -0.008_349_4],
                            [-0.018_150_8, -0.100_578_9, 1.118_729_7],
                        ],
                        rec2020,
                    );
                    let linear = perceptual_gamut_compress(linear);
                    entries.push([
                        srgb_encode(linear[0]),
                        srgb_encode(linear[1]),
                        srgb_encode(linear[2]),
                        0.0,
                    ]);
                }
            }
        }
        Self { size, entries }
    }

    /// Builds a GPU/CPU 3D transform for an RGB matrix-shaper ICC v2/v4
    /// display or output profile. LUT-based ICC profiles are rejected with a
    /// precise error instead of silently treating their device values as sRGB.
    pub fn from_icc(bytes: &[u8], intent: RenderingIntent) -> Result<Self> {
        let profile = MatrixShaperProfile::parse(bytes)?;
        let size = OUTPUT_LUT_EDGE;
        let mut entries = Vec::with_capacity((size * size * size) as usize);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let rgb = [
                        r as f32 / (size - 1) as f32,
                        g as f32 / (size - 1) as f32,
                        b as f32 / (size - 1) as f32,
                    ];
                    let encoded = profile.transform(rgb, intent);
                    entries.push([encoded[0], encoded[1], encoded[2], 0.0]);
                }
            }
        }
        Ok(Self { size, entries })
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// CPU trilinear evaluation for image export. Input is display-referred
    /// linear Rec.2020 in the 0..1 domain; output is encoded device RGB.
    pub fn transform_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        sample_rgb_lut(&self.entries, self.size, rgb)
    }

    pub(crate) fn entries(&self) -> &[[f32; 4]] {
        &self.entries
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProfileGpuLayout {
    pub hue_sat: [u32; 4],
    pub look: [u32; 4],
    pub tone: [u32; 4],
    pub output: [u32; 4],
    pub flags: [u32; 4],
}

impl ProfileGpuLayout {
    fn new(profile: &CameraProfile) -> Self {
        let mut offset = 1u32; // A non-empty storage buffer is required by wgpu.
        let hue_sat = if let Some(map) = &profile.hue_sat_map {
            let out = [map.divisions[0], map.divisions[1], map.divisions[2], offset];
            offset += map.entries.len() as u32;
            out
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
                .hue_sat_map
                .as_ref()
                .map_or(0, |map| map.encoding.shader_value()),
            profile
                .look_table
                .as_ref()
                .map_or(0, |map| map.encoding.shader_value()),
            profile.baseline_exposure_offset.to_bits(),
            0,
        ];
        Self {
            hue_sat,
            look,
            tone,
            output,
            flags,
        }
    }
}

pub(crate) struct ProfileGpuData {
    pub layout: ProfileGpuLayout,
    pub words: Vec<[f32; 4]>,
}

impl ProfileGpuData {
    fn new(profile: &CameraProfile, output: &IccOutputTransform) -> Self {
        let layout = profile.gpu_layout();
        debug_assert_eq!(output.size(), OUTPUT_LUT_EDGE);
        let total = layout.output[3] as usize + output.entries().len();
        let mut words = Vec::with_capacity(total);
        words.push([0.0; 4]);
        if let Some(map) = &profile.hue_sat_map {
            words.extend(
                map.entries
                    .iter()
                    .map(|entry| [entry[0], entry[1], entry[2], 0.0]),
            );
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
}

fn interpolate_optional_matrix_4x3(
    first: Option<[[f32; 3]; 4]>,
    second: Option<[[f32; 3]; 4]>,
    weight: f32,
) -> Option<[[f32; 3]; 4]> {
    match (first, second) {
        (Some(a), Some(b)) => Some(std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                a[row][column] + (b[row][column] - a[row][column]) * weight
            })
        })),
        (Some(matrix), None) | (None, Some(matrix)) => Some(matrix),
        (None, None) => None,
    }
}

fn interpolate_optional_matrix_3x4(
    first: Option<[[f32; 4]; 3]>,
    second: Option<[[f32; 4]; 3]>,
    weight: f32,
) -> Option<[[f32; 4]; 3]> {
    match (first, second) {
        (Some(a), Some(b)) => Some(std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                a[row][column] + (b[row][column] - a[row][column]) * weight
            })
        })),
        (Some(matrix), None) | (None, Some(matrix)) => Some(matrix),
        (None, None) => None,
    }
}

fn interpolate_optional_matrix_4x4(
    first: Option<[[f32; 4]; 4]>,
    second: Option<[[f32; 4]; 4]>,
    weight: f32,
) -> Option<[[f32; 4]; 4]> {
    match (first, second) {
        (Some(a), Some(b)) => Some(std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                a[row][column] + (b[row][column] - a[row][column]) * weight
            })
        })),
        (Some(matrix), None) | (None, Some(matrix)) => Some(matrix),
        (None, None) => None,
    }
}

fn dng_illuminant_cct(illuminant: u16) -> Option<f32> {
    match illuminant {
        1 => Some(5500.0),  // Daylight
        2 => Some(4000.0),  // Fluorescent
        3 => Some(2856.0),  // Tungsten
        4 => Some(5500.0),  // Flash
        9 => Some(5500.0),  // Fine weather
        10 => Some(6500.0), // Cloudy weather
        11 => Some(7500.0), // Shade
        12 => Some(6500.0), // Daylight fluorescent
        13 => Some(5000.0), // Day white fluorescent
        14 => Some(4150.0), // Cool white fluorescent
        15 => Some(3500.0), // White fluorescent
        16 => Some(3000.0), // Warm white fluorescent
        17 => Some(2856.0),
        18 => Some(4874.0),
        19 => Some(6774.0),
        20 => Some(5503.0),
        21 => Some(6504.0),
        22 => Some(7504.0),
        23 => Some(5003.0),
        24 => Some(3200.0),
        _ => None,
    }
}

fn checked_map_len(divisions: [u32; 3]) -> Result<usize> {
    divisions
        .into_iter()
        .try_fold(1usize, |acc, value| acc.checked_mul(value as usize))
        .ok_or_else(|| anyhow!("DCP HSV map dimensions overflow"))
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

    // Thomas algorithm for a natural cubic spline. The endpoint values stay
    // zero, which imposes zero second derivative at x=0 and x=1.
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

// DNG/DCP tag constants.
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

fn profile_from_tags(reader: &mut TiffReader, tags: &[IfdEntry]) -> Result<Option<DcpProfile>> {
    let has_profile = tags.iter().any(|tag| {
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
    });
    if !has_profile {
        return Ok(None);
    }

    let name = read_ascii_tag(reader, tags, PROFILE_NAME)?;
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
        Some(ToneCurve::new(
            tone_values
                .chunks_exact(2)
                .map(|pair| [pair[0], pair[1]])
                .collect(),
        )?)
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
    let baseline_exposure_offset = read_f32_tag(reader, tags, BASELINE_EXPOSURE_OFFSET)?
        .first()
        .copied()
        .unwrap_or(0.0);

    Ok(Some(DcpProfile {
        name,
        camera_calibration_signature,
        calibration_signature,
        matrices,
        hue_sat_maps,
        look_table,
        tone_curve,
        baseline_exposure_offset,
    }))
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
    let entries = values
        .chunks_exact(3)
        .map(|entry| [entry[0], entry[1], entry[2]])
        .collect();
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
struct IfdEntry {
    tag: u16,
    field_type: u16,
    count: u64,
    value_or_offset: u64,
    inline: [u8; 8],
}

struct TiffReader {
    file: File,
    endian: Endian,
    big_tiff: bool,
    first_ifd: u64,
    file_len: u64,
}

impl TiffReader {
    fn new(mut file: File) -> Result<Self> {
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

    fn read_primary_ifd(&mut self) -> Result<Vec<IfdEntry>> {
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
        let mut entries = Vec::with_capacity(count as usize);
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
        if byte_len > 64 * 1024 * 1024 {
            bail!("refusing to allocate oversized TIFF profile tag ({byte_len} bytes)");
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
        let mut bytes = vec![0; byte_len as usize];
        self.file.read_exact(&mut bytes)?;
        self.file.seek(SeekFrom::Start(return_pos))?;
        Ok(bytes)
    }

    fn entry_u32(&mut self, entry: &IfdEntry) -> Result<Vec<u32>> {
        let bytes = self.entry_bytes(entry)?;
        match entry.field_type {
            1 | 6 | 7 => Ok(bytes.into_iter().map(u32::from).collect()),
            3 => Ok(bytes
                .chunks_exact(2)
                .map(|chunk| self.endian.u16([chunk[0], chunk[1]]) as u32)
                .collect()),
            4 | 13 => Ok(bytes
                .chunks_exact(4)
                .map(|chunk| self.endian.u32(chunk.try_into().unwrap()))
                .collect()),
            16 | 18 => bytes
                .chunks_exact(8)
                .map(|chunk| {
                    u32::try_from(self.endian.u64(chunk.try_into().unwrap()))
                        .context("TIFF integer does not fit in u32")
                })
                .collect(),
            _ => bail!("TIFF tag {} is not an integer field", entry.tag),
        }
    }

    fn entry_f32(&mut self, entry: &IfdEntry) -> Result<Vec<f32>> {
        let bytes = self.entry_bytes(entry)?;
        let values: Vec<f32> = match entry.field_type {
            3 => bytes
                .chunks_exact(2)
                .map(|chunk| self.endian.u16([chunk[0], chunk[1]]) as f32)
                .collect(),
            4 => bytes
                .chunks_exact(4)
                .map(|chunk| self.endian.u32(chunk.try_into().unwrap()) as f32)
                .collect(),
            5 => bytes
                .chunks_exact(8)
                .map(|chunk| {
                    let n = self.endian.u32(chunk[0..4].try_into().unwrap());
                    let d = self.endian.u32(chunk[4..8].try_into().unwrap());
                    if d == 0 {
                        f32::NAN
                    } else {
                        n as f32 / d as f32
                    }
                })
                .collect(),
            10 => bytes
                .chunks_exact(8)
                .map(|chunk| {
                    let n = self.endian.i32(chunk[0..4].try_into().unwrap());
                    let d = self.endian.i32(chunk[4..8].try_into().unwrap());
                    if d == 0 {
                        f32::NAN
                    } else {
                        n as f32 / d as f32
                    }
                })
                .collect(),
            11 => bytes
                .chunks_exact(4)
                .map(|chunk| self.endian.f32(chunk.try_into().unwrap()))
                .collect(),
            12 => bytes
                .chunks_exact(8)
                .map(|chunk| self.endian.f64(chunk.try_into().unwrap()) as f32)
                .collect(),
            _ => bail!("TIFF tag {} is not a numeric profile field", entry.tag),
        };
        if values.iter().any(|value| !value.is_finite()) {
            bail!("TIFF profile tag {} contains non-finite values", entry.tag);
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

#[derive(Clone, Debug)]
enum TransferCurve {
    Identity,
    Gamma(f32),
    Sampled(Vec<f32>),
    Parametric { kind: u16, params: Vec<f32> },
}

impl TransferCurve {
    /// ICC TRCs encode device -> PCS. Output conversion needs the inverse.
    fn inverse(&self, linear: f32) -> f32 {
        let target = linear.clamp(0.0, 1.0);
        match self {
            Self::Identity => target,
            Self::Gamma(gamma) => target.powf(1.0 / gamma.max(1e-6)),
            _ => {
                let mut lo = 0.0;
                let mut hi = 1.0;
                for _ in 0..20 {
                    let mid = 0.5 * (lo + hi);
                    if self.forward(mid) < target {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            }
        }
    }

    fn forward(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Identity => x,
            Self::Gamma(gamma) => x.powf(*gamma),
            Self::Sampled(values) => {
                if values.is_empty() {
                    return x;
                }
                let location = x * (values.len() - 1) as f32;
                let i = location.floor() as usize;
                let j = (i + 1).min(values.len() - 1);
                values[i] + (values[j] - values[i]) * (location - i as f32)
            }
            Self::Parametric { kind, params } => parametric_curve(*kind, params, x),
        }
    }

    fn validate(&self) -> Result<()> {
        let first = self.forward(0.0);
        let mut previous = first;
        if !previous.is_finite() {
            bail!("ICC transfer curve produces a non-finite value");
        }
        for step in 1..=256 {
            let value = self.forward(step as f32 / 256.0);
            if !value.is_finite() {
                bail!("ICC transfer curve produces a non-finite value");
            }
            if value + 1e-6 < previous {
                bail!("ICC transfer curve must be monotonic");
            }
            previous = value;
        }
        if previous <= first + 1e-6 {
            bail!("ICC transfer curve has no usable dynamic range");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct MatrixShaperProfile {
    pcs_to_device_linear: [[f32; 3]; 3],
    curves: [TransferCurve; 3],
    media_white: [f32; 3],
}

impl MatrixShaperProfile {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
            bail!("invalid ICC profile header");
        }
        if !matches!(bytes[8], 2 | 4) {
            bail!("ICC output profile must be version 2 or version 4");
        }
        let profile_class = &bytes[12..16];
        if profile_class != b"mntr" && profile_class != b"prtr" && profile_class != b"spac" {
            bail!("ICC profile class must be display, output, or color-space");
        }
        if &bytes[16..20] != b"RGB " || &bytes[20..24] != b"XYZ " {
            bail!("ICC output profile must use RGB device space and XYZ PCS");
        }
        let declared = be_u32(&bytes[0..4]) as usize;
        if declared > bytes.len() || declared < 132 {
            bail!("ICC profile size field is invalid");
        }
        let count = be_u32(&bytes[128..132]) as usize;
        let table_end = 132usize
            .checked_add(
                count
                    .checked_mul(12)
                    .ok_or_else(|| anyhow!("ICC tag table overflow"))?,
            )
            .ok_or_else(|| anyhow!("ICC tag table overflow"))?;
        if table_end > declared {
            bail!("ICC tag table extends outside the profile");
        }
        let mut tags = Vec::with_capacity(count);
        for index in 0..count {
            let base = 132 + index * 12;
            let signature: [u8; 4] = bytes[base..base + 4].try_into().unwrap();
            let offset = be_u32(&bytes[base + 4..base + 8]) as usize;
            let size = be_u32(&bytes[base + 8..base + 12]) as usize;
            let end = offset
                .checked_add(size)
                .ok_or_else(|| anyhow!("ICC tag overflow"))?;
            if offset < table_end || end > declared {
                bail!("ICC tag {:?} points outside the profile", signature);
            }
            tags.push((signature, &bytes[offset..end]));
        }
        if tags
            .iter()
            .any(|(signature, _)| is_icc_lut_transform_signature(signature))
        {
            bail!("LUT-based ICC output profiles are not supported by the built-in matrix-shaper engine");
        }
        let r_xyz = parse_icc_xyz(find_icc_tag(&tags, b"rXYZ")?)?;
        let g_xyz = parse_icc_xyz(find_icc_tag(&tags, b"gXYZ")?)?;
        let b_xyz = parse_icc_xyz(find_icc_tag(&tags, b"bXYZ")?)?;
        let device_to_pcs = [
            [r_xyz[0], g_xyz[0], b_xyz[0]],
            [r_xyz[1], g_xyz[1], b_xyz[1]],
            [r_xyz[2], g_xyz[2], b_xyz[2]],
        ];
        let pcs_to_device_linear =
            invert3(device_to_pcs).ok_or_else(|| anyhow!("ICC RGB colorant matrix is singular"))?;
        let curves = [
            parse_icc_curve(find_icc_tag(&tags, b"rTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"gTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"bTRC")?)?,
        ];
        curves.iter().try_for_each(TransferCurve::validate)?;
        let media_white = find_icc_tag_optional(&tags, b"wtpt")
            .map(parse_icc_xyz)
            .transpose()?
            .unwrap_or(D50_XYZ);
        if media_white.iter().any(|value| *value <= 0.0) {
            bail!("ICC media white point must contain positive XYZ values");
        }
        Ok(Self {
            pcs_to_device_linear,
            curves,
            media_white,
        })
    }

    fn transform(&self, rec2020: [f32; 3], intent: RenderingIntent) -> [f32; 3] {
        const REC2020_TO_XYZ_D65: [[f32; 3]; 3] = [
            [0.636_958_06, 0.144_616_9, 0.168_880_98],
            [0.262_700_2, 0.677_998_07, 0.059_301_72],
            [0.0, 0.028_072_693, 1.060_985_1],
        ];
        const D65_TO_D50: [[f32; 3]; 3] = [
            [1.047_929_8, 0.022_946_8, -0.050_192_2],
            [0.029_627_8, 0.990_434_5, -0.017_073_8],
            [-0.009_243, 0.015_055_2, 0.751_874_3],
        ];
        let mut xyz = mul3(D65_TO_D50, mul3(REC2020_TO_XYZ_D65, rec2020));
        if intent == RenderingIntent::AbsoluteColorimetric {
            for channel in 0..3 {
                xyz[channel] *= self.media_white[channel] / D50_XYZ[channel];
            }
        }
        let mut linear = mul3(self.pcs_to_device_linear, xyz);
        linear = match intent {
            RenderingIntent::Perceptual => perceptual_gamut_compress(linear),
            RenderingIntent::Saturation => saturation_gamut_compress(linear),
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {
                linear.map(|v| v.clamp(0.0, 1.0))
            }
        };
        [
            self.curves[0].inverse(linear[0]),
            self.curves[1].inverse(linear[1]),
            self.curves[2].inverse(linear[2]),
        ]
    }
}

fn is_icc_lut_transform_signature(signature: &[u8; 4]) -> bool {
    matches!(
        signature,
        b"A2B0"
            | b"A2B1"
            | b"A2B2"
            | b"A2B3"
            | b"B2A0"
            | b"B2A1"
            | b"B2A2"
            | b"B2A3"
            | b"D2B0"
            | b"D2B1"
            | b"D2B2"
            | b"D2B3"
            | b"B2D0"
            | b"B2D1"
            | b"B2D2"
            | b"B2D3"
    )
}

fn find_icc_tag<'a>(tags: &'a [([u8; 4], &'a [u8])], signature: &[u8; 4]) -> Result<&'a [u8]> {
    find_icc_tag_optional(tags, signature).ok_or_else(|| {
        anyhow!(
            "ICC profile is missing tag {}",
            String::from_utf8_lossy(signature)
        )
    })
}

fn find_icc_tag_optional<'a>(
    tags: &'a [([u8; 4], &'a [u8])],
    signature: &[u8; 4],
) -> Option<&'a [u8]> {
    tags.iter()
        .find(|(candidate, _)| candidate == signature)
        .map(|(_, data)| *data)
}

fn parse_icc_xyz(data: &[u8]) -> Result<[f32; 3]> {
    if data.len() < 20 || &data[0..4] != b"XYZ " {
        bail!("invalid ICC XYZ tag");
    }
    Ok([
        s15_fixed16(&data[8..12]),
        s15_fixed16(&data[12..16]),
        s15_fixed16(&data[16..20]),
    ])
}

fn parse_icc_curve(data: &[u8]) -> Result<TransferCurve> {
    if data.len() < 12 {
        bail!("truncated ICC TRC tag");
    }
    match &data[0..4] {
        b"curv" => {
            let count = be_u32(&data[8..12]) as usize;
            if count == 0 {
                return Ok(TransferCurve::Identity);
            }
            let end = 12usize
                .checked_add(
                    count
                        .checked_mul(2)
                        .ok_or_else(|| anyhow!("ICC curve overflow"))?,
                )
                .ok_or_else(|| anyhow!("ICC curve overflow"))?;
            if end > data.len() {
                bail!("truncated ICC sampled curve");
            }
            if count == 1 {
                let gamma = be_u16(&data[12..14]) as f32 / 256.0;
                if gamma <= 0.0 {
                    bail!("ICC gamma curve must be positive");
                }
                return Ok(TransferCurve::Gamma(gamma));
            }
            Ok(TransferCurve::Sampled(
                data[12..end]
                    .chunks_exact(2)
                    .map(|chunk| be_u16(chunk) as f32 / 65_535.0)
                    .collect(),
            ))
        }
        b"para" => {
            let kind = be_u16(&data[8..10]);
            let count = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => bail!("unsupported ICC parametric curve type {kind}"),
            };
            let end = 12 + count * 4;
            if end > data.len() {
                bail!("truncated ICC parametric curve");
            }
            let params = data[12..end].chunks_exact(4).map(s15_fixed16).collect();
            Ok(TransferCurve::Parametric { kind, params })
        }
        signature => bail!("unsupported ICC TRC tag type {signature:?}"),
    }
}

fn parametric_curve(kind: u16, p: &[f32], x: f32) -> f32 {
    match kind {
        0 => x.max(0.0).powf(p[0]),
        1 => {
            let [g, a, b] = [p[0], p[1], p[2]];
            if x >= -b / a {
                (a * x + b).powf(g)
            } else {
                0.0
            }
        }
        2 => {
            let [g, a, b, c] = [p[0], p[1], p[2], p[3]];
            if x >= -b / a {
                (a * x + b).powf(g) + c
            } else {
                c
            }
        }
        3 => {
            let [g, a, b, c, d] = [p[0], p[1], p[2], p[3], p[4]];
            if x >= d {
                (a * x + b).powf(g)
            } else {
                c * x
            }
        }
        4 => {
            let [g, a, b, c, d, e, f] = [p[0], p[1], p[2], p[3], p[4], p[5], p[6]];
            if x >= d {
                (a * x + b).powf(g) + e
            } else {
                c * x + f
            }
        }
        _ => x,
    }
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes(bytes.try_into().unwrap())
}
fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}
fn s15_fixed16(bytes: &[u8]) -> f32 {
    i32::from_be_bytes(bytes.try_into().unwrap()) as f32 / 65_536.0
}

fn mul3(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

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

fn sample_rgb_lut(entries: &[[f32; 4]], size: u32, rgb: [f32; 3]) -> [f32; 3] {
    let edge = size.max(2);
    let coord = rgb.map(|v| v.clamp(0.0, 1.0) * (edge - 1) as f32);
    let lo = coord.map(|v| v.floor() as u32);
    let hi = lo.map(|v| (v + 1).min(edge - 1));
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
    // Spell out vector interpolation to keep each channel independent.
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

#[cfg(test)]
mod tests {
    use super::{CameraProfile, HsvMap, IccOutputTransform, ProfileEncoding, ToneCurve};

    #[test]
    fn dual_illuminant_maps_interpolate_entrywise() {
        let a = HsvMap::new(
            [1, 2, 1],
            vec![[0.0, 1.0, 1.0], [10.0, 0.5, 1.0]],
            ProfileEncoding::Linear,
        )
        .unwrap();
        let b = HsvMap::new(
            [1, 2, 1],
            vec![[20.0, 2.0, 1.0], [30.0, 1.5, 2.0]],
            ProfileEncoding::Linear,
        )
        .unwrap();
        let mixed = HsvMap::interpolate(&a, &b, 0.25).unwrap();
        assert_eq!(mixed.entries[0], [5.0, 1.25, 1.0]);
    }

    #[test]
    fn tone_curve_sampling_is_monotonic_for_monotonic_points() {
        let curve = ToneCurve::new(vec![[0.0, 0.0], [0.2, 0.08], [0.7, 0.82], [1.0, 1.0]]).unwrap();
        let lut = curve.sampled_lut(256);
        assert!(lut.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6));
    }

    #[test]
    fn default_srgb_output_lut_preserves_neutral_order() {
        let lut = IccOutputTransform::srgb();
        let low = lut.transform_rgb([0.1; 3]);
        let high = lut.transform_rgb([0.5; 3]);
        assert!(low[0] < high[0]);
        assert!((low[0] - low[1]).abs() < 0.02);
    }

    #[test]
    fn gpu_layout_offsets_are_contiguous() {
        let profile = CameraProfile {
            hue_sat_map: Some(
                HsvMap::new([1, 2, 1], vec![[0.0, 1.0, 1.0]; 2], ProfileEncoding::Linear).unwrap(),
            ),
            tone_curve: Some(ToneCurve::new(vec![[0.0, 0.0], [1.0, 1.0]]).unwrap()),
            ..Default::default()
        };
        let data = profile.gpu_data(&IccOutputTransform::srgb());
        assert_eq!(data.layout.hue_sat[3], 1);
        assert_eq!(data.layout.tone[1], 3);
        assert_eq!(
            data.words.len(),
            data.layout.output[3] as usize + 33usize.pow(3)
        );
    }
}
