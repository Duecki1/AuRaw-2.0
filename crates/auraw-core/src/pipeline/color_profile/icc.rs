use super::*;

#[derive(Clone, Debug)]
enum TransferCurve {
    Identity,
    Gamma(f32),
    Sampled(Vec<f32>),
    Parametric { kind: u16, params: Vec<f32> },
}

impl TransferCurve {
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
    curves: [TransferCurve; 3],
}

impl MatrixShaperProfile {
    fn parse_input(bytes: &[u8]) -> Result<Self> {
        Self::parse_impl(bytes)
    }

    fn parse_impl(bytes: &[u8]) -> Result<Self> {
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
        let r_xyz = parse_icc_xyz(find_icc_tag(&tags, b"rXYZ")?)?;
        let g_xyz = parse_icc_xyz(find_icc_tag(&tags, b"gXYZ")?)?;
        let b_xyz = parse_icc_xyz(find_icc_tag(&tags, b"bXYZ")?)?;
        let device_to_pcs = [
            [r_xyz[0], g_xyz[0], b_xyz[0]],
            [r_xyz[1], g_xyz[1], b_xyz[1]],
            [r_xyz[2], g_xyz[2], b_xyz[2]],
        ];
        let curves = [
            parse_icc_curve(find_icc_tag(&tags, b"rTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"gTRC")?)?,
            parse_icc_curve(find_icc_tag(&tags, b"bTRC")?)?,
        ];
        curves.iter().try_for_each(TransferCurve::validate)?;
        Ok(Self {
            device_to_pcs,
            curves,
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
}

pub(super) fn convert_input_rgb_to_rec2020(bytes: &[u8], rgb: &mut [f32]) -> Result<()> {
    if !rgb.len().is_multiple_of(3) {
        bail!("ICC source RGB buffer length must be divisible by three");
    }
    let profile = MatrixShaperProfile::parse_input(bytes)?;
    for pixel in rgb.chunks_exact_mut(3) {
        let converted = profile.transform_input_to_rec2020([pixel[0], pixel[1], pixel[2]]);
        pixel.copy_from_slice(&converted);
    }
    Ok(())
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
    fn input_matrix_profile_round_trips_linear_rec2020() {
        let device_to_pcs = [
            [0.673_515_44, 0.165_697_22, 0.125_083_01],
            [0.279_059_02, 0.675_318_06, 0.045_622_99],
            [-0.001_932_4, 0.029_977_84, 0.797_059_24],
        ];
        let profile = MatrixShaperProfile {
            device_to_pcs,
            curves: [
                TransferCurve::Identity,
                TransferCurve::Identity,
                TransferCurve::Identity,
            ],
        };
        let source = [0.18, 0.42, 0.91];
        let converted = profile.transform_input_to_rec2020(source);
        for channel in 0..3 {
            assert!((converted[channel] - source[channel]).abs() < 5e-5);
        }
    }
}
