use super::*;

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
pub(super) struct MatrixShaperProfile {
    pcs_to_device_linear: [[f32; 3]; 3],
    curves: [TransferCurve; 3],
    media_white: [f32; 3],
}

impl MatrixShaperProfile {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
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
}
