// Matrix-shaper ICC parsing is shared by TIFF input color management and the
// Android display-profile fallback. Desktop LUT profiles can additionally fall
// through to LCMS2 when they cannot be represented as a matrix + per-channel TRC.
#![cfg_attr(test, allow(dead_code))]

use super::*;

#[derive(Clone, Debug)]
enum TransferCurve {
    Identity,
    Gamma(f32),
    Sampled(Vec<f32>),
    Parametric { kind: u16, params: Vec<f32> },
}

impl TransferCurve {
    #[cfg(any(target_os = "android", test))]
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

    fn forward_extended(&self, x: f32) -> f32 {
        match self {
            Self::Identity => x,
            Self::Gamma(gamma) => x.signum() * x.abs().powf(*gamma),
            Self::Sampled(values) => {
                if values.is_empty() {
                    return x;
                }
                if values.len() == 1 {
                    return values[0];
                }
                if x <= 0.0 {
                    let slope = (values[1] - values[0]) * (values.len() - 1) as f32;
                    return values[0] + slope * x;
                }
                if x >= 1.0 {
                    let last = values.len() - 1;
                    let slope = (values[last] - values[last - 1]) * last as f32;
                    return values[last] + slope * (x - 1.0);
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
pub(super) struct MatrixShaperProfile {
    device_to_pcs: [[f32; 3]; 3],
    #[cfg(any(target_os = "android", test))]
    pcs_to_device_linear: [[f32; 3]; 3],
    curves: [TransferCurve; 3],
    #[cfg(any(target_os = "android", test))]
    media_white: [f32; 3],
}

impl MatrixShaperProfile {
    #[cfg(any(target_os = "android", test))]
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_impl(bytes, true)
    }

    fn parse_input(bytes: &[u8]) -> Result<Self> {
        // RGB working-space profiles sometimes carry optional A2B/B2A tables in
        // addition to authoritative matrix/TRC tags. For TIFF source conversion
        // the matrix/TRC representation is sufficient and keeps Android native.
        Self::parse_impl(bytes, false)
    }

    fn parse_impl(bytes: &[u8], reject_lut_tags: bool) -> Result<Self> {
        if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
            bail!("invalid ICC profile header");
        }
        if !matches!(bytes[8], 2 | 4) {
            bail!("ICC profile must be version 2 or version 4");
        }
        let profile_class = &bytes[12..16];
        if profile_class != b"mntr"
            && profile_class != b"prtr"
            && profile_class != b"spac"
            && profile_class != b"scnr"
        {
            bail!("ICC profile class must be input, display, output, or color-space");
        }
        if &bytes[16..20] != b"RGB " || &bytes[20..24] != b"XYZ " {
            bail!("ICC matrix-shaper profile must use RGB device space and XYZ PCS");
        }
        let declared = be_u32(&bytes[0..4], "ICC profile size")? as usize;
        if declared > bytes.len() || declared < 132 {
            bail!("ICC profile size field is invalid");
        }
        let count = be_u32(&bytes[128..132], "ICC tag count")? as usize;
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
            let signature = checked_array(&bytes[base..base + 4], "ICC tag signature")?;
            let offset = be_u32(&bytes[base + 4..base + 8], "ICC tag offset")? as usize;
            let size = be_u32(&bytes[base + 8..base + 12], "ICC tag size")? as usize;
            let end = offset
                .checked_add(size)
                .ok_or_else(|| anyhow!("ICC tag overflow"))?;
            if offset < table_end || end > declared {
                bail!("ICC tag {:?} points outside the profile", signature);
            }
            tags.push((signature, &bytes[offset..end]));
        }
        if reject_lut_tags
            && tags
                .iter()
                .any(|(signature, _)| is_icc_lut_transform_signature(signature))
        {
            bail!("LUT-based ICC profiles are not supported by the built-in matrix-shaper engine");
        }
        let r_xyz = parse_icc_xyz(find_icc_tag(&tags, b"rXYZ")?)?;
        let g_xyz = parse_icc_xyz(find_icc_tag(&tags, b"gXYZ")?)?;
        let b_xyz = parse_icc_xyz(find_icc_tag(&tags, b"bXYZ")?)?;
        let device_to_pcs = [
            [r_xyz[0], g_xyz[0], b_xyz[0]],
            [r_xyz[1], g_xyz[1], b_xyz[1]],
            [r_xyz[2], g_xyz[2], b_xyz[2]],
        ];
        #[cfg(any(target_os = "android", test))]
        let pcs_to_device_linear =
            invert3(device_to_pcs).ok_or_else(|| anyhow!("ICC RGB colorant matrix is singular"))?;
        let curves = [
            parse_icc_curve(find_icc_tag(&tags, b"rTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"gTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"bTRC")?)?,
        ];
        curves.iter().try_for_each(TransferCurve::validate)?;
        #[cfg(any(target_os = "android", test))]
        let media_white = {
            let media_white = find_icc_tag_optional(&tags, b"wtpt")
                .map(parse_icc_xyz)
                .transpose()?
                .unwrap_or(D50_XYZ);
            if media_white.iter().any(|value| *value <= 0.0) {
                bail!("ICC media white point must contain positive XYZ values");
            }
            media_white
        };
        Ok(Self {
            device_to_pcs,
            #[cfg(any(target_os = "android", test))]
            pcs_to_device_linear,
            curves,
            #[cfg(any(target_os = "android", test))]
            media_white,
        })
    }

    fn transform_input_to_rec2020(&self, encoded: [f32; 3]) -> [f32; 3] {
        const D50_TO_D65: [[f32; 3]; 3] = [
            [0.955_473_4, -0.023_098_5, 0.063_259_3],
            [-0.028_369_7, 1.009_995_5, 0.021_041_4],
            [0.012_314_0, -0.020_507_7, 1.330_365_9],
        ];
        const XYZ_D65_TO_REC2020: [[f32; 3]; 3] = [
            [1.716_651_1, -0.355_670_8, -0.253_366_3],
            [-0.666_684_3, 1.616_481_2, 0.015_768_5],
            [0.017_639_9, -0.042_770_6, 0.942_103_1],
        ];
        let linear = [
            self.curves[0].forward_extended(encoded[0]),
            self.curves[1].forward_extended(encoded[1]),
            self.curves[2].forward_extended(encoded[2]),
        ];
        let xyz_d50 = mul3(self.device_to_pcs, linear);
        mul3(XYZ_D65_TO_REC2020, mul3(D50_TO_D65, xyz_d50))
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn transform(&self, rec2020: [f32; 3], intent: RenderingIntent) -> [f32; 3] {
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
                xyz[channel] *= D50_XYZ[channel] / self.media_white[channel];
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

pub(super) fn convert_input_rgb_to_rec2020(bytes: &[u8], rgb: &mut [f32]) -> Result<()> {
    if !rgb.len().is_multiple_of(3) {
        bail!("ICC source RGB buffer length must be divisible by three");
    }
    match MatrixShaperProfile::parse_input(bytes) {
        Ok(profile) => {
            for pixel in rgb.chunks_exact_mut(3) {
                let converted = profile.transform_input_to_rec2020([pixel[0], pixel[1], pixel[2]]);
                pixel.copy_from_slice(&converted);
            }
            Ok(())
        }
        Err(matrix_error) => {
            #[cfg(not(target_os = "android"))]
            {
                convert_input_rgb_to_rec2020_lcms(bytes, rgb).map_err(|lcms_error| {
                    anyhow!(
                        "embedded ICC profile is unsupported by both matrix-shaper and LCMS2 paths: matrix-shaper: {matrix_error}; LCMS2: {lcms_error}"
                    )
                })
            }
            #[cfg(target_os = "android")]
            {
                Err(anyhow!(
                    "embedded ICC profile is not a supported RGB matrix-shaper profile: {matrix_error}"
                ))
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn convert_input_rgb_to_rec2020_lcms(bytes: &[u8], rgb: &mut [f32]) -> Result<()> {
    use lcms2::{
        CIExyY, CIExyYTRIPLE, Flags, Intent, PixelFormat, Profile, ToneCurve as LcmsToneCurve,
        Transform,
    };

    let input = Profile::new_icc(bytes)
        .map_err(|error| anyhow!("LCMS2 could not open embedded ICC profile: {error}"))?;
    let white = CIExyY {
        x: 0.3127,
        y: 0.3290,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.708,
            y: 0.292,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.170,
            y: 0.797,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.131,
            y: 0.046,
            Y: 1.0,
        },
    };
    let linear = LcmsToneCurve::new(1.0);
    let output = Profile::new_rgb(&white, &primaries, &[&linear, &linear, &linear])
        .map_err(|error| anyhow!("LCMS2 could not create linear Rec.2020 profile: {error}"))?;
    let transform: Transform<[f32; 3], [f32; 3]> = Transform::new_flags(
        &input,
        PixelFormat::RGB_FLT,
        &output,
        PixelFormat::RGB_FLT,
        Intent::RelativeColorimetric,
        Flags::HIGHRES_PRECALC,
    )
    .map_err(|error| anyhow!("LCMS2 could not build embedded-profile transform: {error}"))?;

    const CHUNK_PIXELS: usize = 16_384;
    let mut source = Vec::<[f32; 3]>::with_capacity(CHUNK_PIXELS);
    let mut destination = vec![[0.0_f32; 3]; CHUNK_PIXELS];
    for chunk in rgb.chunks_mut(CHUNK_PIXELS * 3) {
        source.clear();
        source.extend(
            chunk
                .chunks_exact(3)
                .map(|pixel| [pixel[0], pixel[1], pixel[2]]),
        );
        transform.transform_pixels(&source, &mut destination[..source.len()]);
        for (pixel, converted) in chunk
            .chunks_exact_mut(3)
            .zip(destination[..source.len()].iter())
        {
            if converted.iter().any(|value| !value.is_finite()) {
                bail!("LCMS2 embedded-profile transform produced a non-finite value");
            }
            pixel.copy_from_slice(converted);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub(super) fn build_lcms_output_lut(
    bytes: &[u8],
    intent: RenderingIntent,
    size: u32,
) -> Result<Vec<[f32; 4]>> {
    use lcms2::{
        CIExyY, CIExyYTRIPLE, Flags, Intent, PixelFormat, Profile, ToneCurve as LcmsToneCurve,
        Transform,
    };

    if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
        bail!("invalid ICC profile header");
    }

    let output = Profile::new_icc(bytes)
        .map_err(|error| anyhow!("LCMS2 could not open ICC profile: {error}"))?;
    let white = CIExyY {
        x: 0.3127,
        y: 0.3290,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.708,
            y: 0.292,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.170,
            y: 0.797,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.131,
            y: 0.046,
            Y: 1.0,
        },
    };
    let linear = LcmsToneCurve::new(1.0);
    let input =
        Profile::new_rgb(&white, &primaries, &[&linear, &linear, &linear]).map_err(|error| {
            anyhow!("LCMS2 could not create linear Rec.2020 input profile: {error}")
        })?;
    let absolute_colorimetric = intent == RenderingIntent::AbsoluteColorimetric;
    let intent = match intent {
        RenderingIntent::Perceptual => Intent::Perceptual,
        RenderingIntent::RelativeColorimetric => Intent::RelativeColorimetric,
        RenderingIntent::Saturation => Intent::Saturation,
        RenderingIntent::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
    };
    let flags = if absolute_colorimetric {
        Flags::HIGHRES_PRECALC
    } else {
        Flags::HIGHRES_PRECALC | Flags::BLACKPOINT_COMPENSATION
    };
    let transform: Transform<[f32; 3], [f32; 3]> = Transform::new_flags(
        &input,
        PixelFormat::RGB_FLT,
        &output,
        PixelFormat::RGB_FLT,
        intent,
        flags,
    )
    .map_err(|error| anyhow!("LCMS2 could not build display transform: {error}"))?;

    let mut source = Vec::with_capacity((size * size * size) as usize);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                source.push([
                    output_lut_linear_node(r, size),
                    output_lut_linear_node(g, size),
                    output_lut_linear_node(b, size),
                ]);
            }
        }
    }
    let mut destination = vec![[0.0_f32; 3]; source.len()];
    transform.transform_pixels(&source, &mut destination);

    if destination
        .iter()
        .flat_map(|rgb| rgb.iter())
        .any(|value| !value.is_finite())
    {
        bail!("LCMS2 display transform produced a non-finite value");
    }

    Ok(destination
        .into_iter()
        .map(|rgb| {
            [
                rgb[0].clamp(0.0, 1.0),
                rgb[1].clamp(0.0, 1.0),
                rgb[2].clamp(0.0, 1.0),
                0.0,
            ]
        })
        .collect())
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
        s15_fixed16(&data[8..12], "ICC X coordinate")?,
        s15_fixed16(&data[12..16], "ICC Y coordinate")?,
        s15_fixed16(&data[16..20], "ICC Z coordinate")?,
    ])
}

fn parse_icc_curve(data: &[u8]) -> Result<TransferCurve> {
    if data.len() < 12 {
        bail!("truncated ICC TRC tag");
    }
    match &data[0..4] {
        b"curv" => {
            let count = be_u32(&data[8..12], "ICC curve sample count")? as usize;
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
                let gamma = be_u16(&data[12..14], "ICC gamma")? as f32 / 256.0;
                if gamma <= 0.0 {
                    bail!("ICC gamma curve must be positive");
                }
                return Ok(TransferCurve::Gamma(gamma));
            }
            let samples = data[12..end]
                .chunks_exact(2)
                .map(|chunk| Ok(be_u16(chunk, "ICC sampled curve value")? as f32 / 65_535.0))
                .collect::<Result<Vec<_>>>()?;
            Ok(TransferCurve::Sampled(samples))
        }
        b"para" => {
            let kind = be_u16(&data[8..10], "ICC parametric curve type")?;
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
            let params = data[12..end]
                .chunks_exact(4)
                .map(|chunk| s15_fixed16(chunk, "ICC parametric curve coefficient"))
                .collect::<Result<Vec<_>>>()?;
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

fn checked_array<const N: usize>(bytes: &[u8], label: &str) -> Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("{label} requires exactly {N} bytes, got {}", bytes.len()))
}

fn be_u16(bytes: &[u8], label: &str) -> Result<u16> {
    Ok(u16::from_be_bytes(checked_array(bytes, label)?))
}

fn be_u32(bytes: &[u8], label: &str) -> Result<u32> {
    Ok(u32::from_be_bytes(checked_array(bytes, label)?))
}

fn s15_fixed16(bytes: &[u8], label: &str) -> Result<f32> {
    Ok(i32::from_be_bytes(checked_array(bytes, label)?) as f32 / 65_536.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_intent_maps_media_white_to_device_white() {
        let profile = MatrixShaperProfile {
            device_to_pcs: [
                [D50_XYZ[0], 0.0, 0.0],
                [0.0, D50_XYZ[1], 0.0],
                [0.0, 0.0, D50_XYZ[2]],
            ],
            pcs_to_device_linear: [
                [1.0 / D50_XYZ[0], 0.0, 0.0],
                [0.0, 1.0 / D50_XYZ[1], 0.0],
                [0.0, 0.0, 1.0 / D50_XYZ[2]],
            ],
            curves: [
                TransferCurve::Identity,
                TransferCurve::Identity,
                TransferCurve::Identity,
            ],
            media_white: D50_XYZ.map(|value| value * 0.8),
        };

        let relative = profile.transform([0.8; 3], RenderingIntent::RelativeColorimetric);
        let absolute = profile.transform([0.8; 3], RenderingIntent::AbsoluteColorimetric);

        for channel in 0..3 {
            assert!((relative[channel] - 0.8).abs() < 2e-4);
            assert!((absolute[channel] - 1.0).abs() < 2e-4);
        }
    }

    #[test]
    fn input_matrix_profile_round_trips_linear_rec2020() {
        // Rec.2020 D65 colorants adapted into the ICC D50 PCS.
        let device_to_pcs = [
            [0.673_515_44, 0.165_697_22, 0.125_083_01],
            [0.279_059_02, 0.675_318_06, 0.045_622_99],
            [-0.001_932_4, 0.029_977_84, 0.797_059_24],
        ];
        let profile = MatrixShaperProfile {
            device_to_pcs,
            pcs_to_device_linear: invert3(device_to_pcs).unwrap(),
            curves: [
                TransferCurve::Identity,
                TransferCurve::Identity,
                TransferCurve::Identity,
            ],
            media_white: D50_XYZ,
        };
        let source = [0.18, 0.42, 0.91];
        let converted = profile.transform_input_to_rec2020(source);
        for channel in 0..3 {
            assert!((converted[channel] - source[channel]).abs() < 5e-5);
        }
    }
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug)]
pub(super) struct DiscoveredDisplayProfile {
    pub bytes: Vec<u8>,
    pub label: String,
    pub source: String,
}

#[cfg(not(target_os = "android"))]
pub(super) fn read_display_profile_file(
    path: &std::path::Path,
) -> Result<DiscoveredDisplayProfile> {
    const MAX_DISPLAY_PROFILE_BYTES: u64 = 32 * 1024 * 1024;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("could not inspect display ICC {}", path.display()))?;
    if !metadata.is_file() || metadata.len() < 132 || metadata.len() > MAX_DISPLAY_PROFILE_BYTES {
        bail!("display ICC file has an invalid size");
    }
    let bytes = std::fs::read(path)
        .with_context(|| format!("could not read display ICC {}", path.display()))?;
    if &bytes[36..40] != b"acsp" {
        bail!("selected display profile is not an ICC profile");
    }
    Ok(DiscoveredDisplayProfile {
        label: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Display ICC")
            .to_owned(),
        source: path.display().to_string(),
        bytes,
    })
}

#[cfg(not(target_os = "android"))]
pub(super) fn discover_display_profile(
    screen_point: Option<[i32; 2]>,
) -> Result<Option<DiscoveredDisplayProfile>> {
    if let Some(path) = std::env::var_os("AURAW_DISPLAY_ICC").map(std::path::PathBuf::from) {
        return read_display_profile_file(&path).map(Some);
    }

    #[cfg(target_os = "windows")]
    if let Some(path) = windows_display_profile_path(screen_point)? {
        return read_display_profile_file(&path).map(Some);
    }

    #[cfg(target_os = "macos")]
    if let Some(profile) = macos_display_profile(screen_point)? {
        return Ok(Some(profile));
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    if let Some(bytes) = x11_display_profile_bytes(screen_point)? {
        if bytes.len() >= 132 && &bytes[36..40] == b"acsp" {
            return Ok(Some(DiscoveredDisplayProfile {
                bytes,
                label: "X11 monitor ICC profile".to_owned(),
                source: "X11 _ICC_PROFILE property".to_owned(),
            }));
        }
    }

    Ok(None)
}

#[cfg(target_os = "windows")]
fn windows_display_profile_path(
    screen_point: Option<[i32; 2]>,
) -> Result<Option<std::path::PathBuf>> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MonitorInfoExW {
        cb_size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
        device: [u16; 32],
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MonitorFromPoint(point: Point, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(monitor: *mut c_void, info: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateDCW(
            driver: *const u16,
            device: *const u16,
            output: *const u16,
            init_data: *const c_void,
        ) -> *mut c_void;
        fn DeleteDC(dc: *mut c_void) -> i32;
        fn GetICMProfileW(dc: *mut c_void, size: *mut u32, filename: *mut u16) -> i32;
    }

    let [x, y] = screen_point.unwrap_or([0, 0]);
    let monitor = unsafe { MonitorFromPoint(Point { x, y }, 2) }; // MONITOR_DEFAULTTONEAREST
    if monitor.is_null() {
        return Ok(None);
    }
    let mut info = MonitorInfoExW {
        cb_size: std::mem::size_of::<MonitorInfoExW>() as u32,
        monitor: Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        work: Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        flags: 0,
        device: [0; 32],
    };
    if unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MonitorInfoExW).cast()) } == 0 {
        return Ok(None);
    }

    const DISPLAY: [u16; 8] = [
        'D' as u16, 'I' as u16, 'S' as u16, 'P' as u16, 'L' as u16, 'A' as u16, 'Y' as u16, 0,
    ];
    let dc = unsafe {
        CreateDCW(
            DISPLAY.as_ptr(),
            info.device.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if dc.is_null() {
        return Ok(None);
    }

    let mut capacity = 32_768_u32;
    let mut filename = vec![0_u16; capacity as usize];
    let success = unsafe { GetICMProfileW(dc, &mut capacity, filename.as_mut_ptr()) } != 0;
    unsafe {
        DeleteDC(dc);
    }
    if !success || capacity == 0 {
        return Ok(None);
    }
    let end = filename
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filename.len());
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&filename[..end]));
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    if path.is_absolute() {
        return Ok(Some(path));
    }

    let resolved = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .map(|windows| {
            windows
                .join("System32")
                .join("spool")
                .join("drivers")
                .join("color")
                .join(&path)
        });
    Ok(resolved.or(Some(path)))
}

#[cfg(target_os = "macos")]
fn macos_display_profile(
    screen_point: Option<[i32; 2]>,
) -> Result<Option<DiscoveredDisplayProfile>> {
    use std::ffi::c_void;

    type CGDirectDisplayId = u32;
    type CGError = i32;
    type CFIndex = isize;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> CGDirectDisplayId;
        fn CGGetDisplaysWithPoint(
            point: CGPoint,
            max_displays: u32,
            displays: *mut CGDirectDisplayId,
            matching_display_count: *mut u32,
        ) -> CGError;
        fn CGDisplayCopyColorSpace(display: CGDirectDisplayId) -> *mut c_void;
        fn CGColorSpaceCopyICCData(space: *mut c_void) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataGetLength(data: *const c_void) -> CFIndex;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(value: *const c_void);
    }

    let display = unsafe {
        let mut display = 0;
        let mut count = 0;
        if let Some([x, y]) = screen_point {
            let status = CGGetDisplaysWithPoint(
                CGPoint {
                    x: x as f64,
                    y: y as f64,
                },
                1,
                &mut display,
                &mut count,
            );
            if status != 0 || count == 0 {
                display = CGMainDisplayID();
            }
        } else {
            display = CGMainDisplayID();
        }
        display
    };

    let color_space = unsafe { CGDisplayCopyColorSpace(display) };
    if color_space.is_null() {
        return Ok(None);
    }
    let icc_data = unsafe { CGColorSpaceCopyICCData(color_space) };
    unsafe { CFRelease(color_space as *const c_void) };
    if icc_data.is_null() {
        return Ok(None);
    }

    let bytes = unsafe {
        let length = CFDataGetLength(icc_data);
        let pointer = CFDataGetBytePtr(icc_data);
        let result = if length >= 132 && length <= 32 * 1024 * 1024 && !pointer.is_null() {
            std::slice::from_raw_parts(pointer, length as usize).to_vec()
        } else {
            Vec::new()
        };
        CFRelease(icc_data);
        result
    };
    if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
        return Ok(None);
    }

    Ok(Some(DiscoveredDisplayProfile {
        bytes,
        label: format!("macOS display {display} ICC profile"),
        source: "CoreGraphics active display color space".to_owned(),
    }))
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn x11_display_profile_bytes(screen_point: Option<[i32; 2]>) -> Result<Option<Vec<u8>>> {
    use std::process::Command;
    if std::env::var_os("DISPLAY").is_none() {
        return Ok(None);
    }
    let monitor_index = x11_monitor_index(screen_point).unwrap_or(0);
    let property = if monitor_index == 0 {
        "_ICC_PROFILE".to_owned()
    } else {
        format!("_ICC_PROFILE_{monitor_index}")
    };
    let output = match Command::new("xprop")
        .args(["-root", "-notype", &property])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Ok(None),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some((_, values)) = text.split_once('=') else {
        return Ok(None);
    };
    let bytes = parse_xprop_icc_values(values);
    Ok((!bytes.is_empty()).then_some(bytes))
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn parse_xprop_icc_values(values: &str) -> Vec<u8> {
    values
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter_map(|value| {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            let parsed = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .and_then(|hex| u16::from_str_radix(hex, 16).ok())
                .or_else(|| value.parse::<u16>().ok())?;
            u8::try_from(parsed).ok()
        })
        .collect()
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn x11_monitor_index(screen_point: Option<[i32; 2]>) -> Option<usize> {
    use std::process::Command;
    let output = Command::new("xrandr").arg("--listmonitors").output().ok()?;
    if !output.status.success() {
        return None;
    }
    x11_monitor_index_from_text(&String::from_utf8_lossy(&output.stdout), screen_point?)
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn x11_monitor_index_from_text(text: &str, [px, py]: [i32; 2]) -> Option<usize> {
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let Some(index) = fields
            .next()
            .and_then(|field| field.trim_end_matches(':').parse::<usize>().ok())
        else {
            continue;
        };
        let Some(geometry) = fields
            .find(|field| field.contains('x') && (field.contains('+') || field.contains('-')))
        else {
            continue;
        };
        let Some((left, rest)) = geometry.split_once('x') else {
            continue;
        };
        let Some(width) = left
            .split('/')
            .next()
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        let Some(height) = rest
            .split('/')
            .next()
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        let Some(slash) = rest.find('/') else {
            continue;
        };
        let offsets = &rest[slash + 1..];
        let Some(first_sign) = offsets.find(['+', '-']) else {
            continue;
        };
        let offsets = &offsets[first_sign..];
        let Some(second_sign_rel) = offsets[1..].find(['+', '-']).map(|offset| offset + 1) else {
            continue;
        };
        let Some(x) = offsets[..second_sign_rel].parse::<i32>().ok() else {
            continue;
        };
        let Some(y) = offsets[second_sign_rel..].parse::<i32>().ok() else {
            continue;
        };
        if px >= x && px < x + width && py >= y && py < y + height {
            return Some(index);
        }
    }
    None
}

#[cfg(all(test, unix, not(target_os = "macos"), not(target_os = "android")))]
mod x11_tests {
    use super::*;

    #[test]
    fn chooses_monitor_with_negative_origin() {
        let text = "Monitors: 2\n 0: +HDMI-1 1920/510x1080/287-1920+0 HDMI-1\n 1: +*DP-1 2560/600x1440/340+0+0 DP-1\n";
        assert_eq!(x11_monitor_index_from_text(text, [-100, 500]), Some(0));
        assert_eq!(x11_monitor_index_from_text(text, [1200, 500]), Some(1));
    }

    #[test]
    fn parses_decimal_and_hex_xprop_payloads() {
        assert_eq!(parse_xprop_icc_values(" 0, 255, 17 "), vec![0, 255, 17]);
        assert_eq!(
            parse_xprop_icc_values(" 0x00, 0xff, 0x11 "),
            vec![0, 255, 17]
        );
    }
}
