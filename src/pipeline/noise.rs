use super::raw_loader::CompactPixelMap;
use serde::{Deserialize, Serialize};

/// Signal-dependent RAW noise model in normalized sensor units.
///
/// For each CFA plane, variance is modeled as `shot[channel] * signal +
/// read[channel]`. The coefficients are estimated from flat, same-color
/// second differences in the active mosaic, with a conservative ISO-derived
/// fallback when the capture does not contain enough usable samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseProfile {
    pub shot: [f32; 4],
    pub read: [f32; 4],
    pub confidence: f32,
    pub green2_present: bool,
}

impl Default for NoiseProfile {
    fn default() -> Self {
        // Roughly half a 16-bit code of variance. This is intentionally tiny:
        // an unavailable profile must not cause visible smoothing by itself.
        let quantization = (0.5f32 / 65_535.0).powi(2);
        Self {
            shot: [0.0; 4],
            read: [quantization; 4],
            confidence: 0.0,
            green2_present: false,
        }
    }
}

impl NoiseProfile {
    /// Estimate one heteroscedastic `a*signal+b` model per CFA plane directly
    /// from the decoded mosaic. Sampling is bounded so RAW open time remains
    /// effectively independent of megapixel count.
    pub fn estimate(
        width: u32,
        height: u32,
        raw_pixels: &[u16],
        color_indices: &CompactPixelMap<u8>,
        black_levels_per_pixel: &CompactPixelMap<f32>,
        white_levels: [f32; 4],
        iso_speed: f32,
        cfa_period: u32,
    ) -> Self {
        const BIN_COUNT: usize = 20;
        const TARGET_ANCHORS: u64 = 48_000;
        const MIN_BIN_SAMPLES: usize = 24;

        let green2_present = cfa_plane_present(width, height, color_indices, cfa_period, 3);
        let expected_len = (width as usize).saturating_mul(height as usize);

        if width < cfa_period.saturating_mul(3) + 1
            || height < cfa_period.saturating_mul(3) + 1
            || raw_pixels.len() < expected_len
            || color_indices.len() < expected_len
            || black_levels_per_pixel.len() < expected_len
        {
            return Self::iso_fallback(iso_speed, white_levels, green2_present);
        }

        let pixels = u64::from(width) * u64::from(height);
        let coarse = ((pixels / TARGET_ANCHORS).max(1) as f64).sqrt().ceil() as u32;
        // Keep the sampling stride coprime with the CFA period. Otherwise a
        // perfectly reasonable even stride can repeatedly hit only one Bayer
        // phase (or a subset of X-Trans phases) and bias the fitted profile.
        let radius = cfa_period.max(1);
        let stride = coprime_stride(coarse.max(1), radius);
        let mut bins: [[Vec<f32>; BIN_COUNT]; 4] =
            std::array::from_fn(|_| std::array::from_fn(|_| Vec::new()));

        let normalized = |x: u32, y: u32, channel: usize| -> Option<f32> {
            let index = (y * width + x) as usize;
            let raw = f32::from(*raw_pixels.get(index)?);
            let black = *black_levels_per_pixel.get(index)?;
            let white = white_levels[channel].max(black + 1.0);
            Some(((raw - black) / (white - black)).clamp(0.0, 1.25))
        };

        let y_start = radius;
        let y_end = height.saturating_sub(radius);
        let x_start = radius;
        let x_end = width.saturating_sub(radius);
        let mut y = y_start;
        while y < y_end {
            let mut x = x_start;
            while x < x_end {
                let index = (y * width + x) as usize;
                let channel = usize::from(color_indices[index]).min(3);
                let center = match normalized(x, y, channel) {
                    Some(v) if v < 0.985 => v,
                    _ => {
                        x = x.saturating_add(stride);
                        continue;
                    }
                };

                // Same-CFA second differences suppress first-order scene gradients.
                // Taking the flatter of horizontal/vertical estimates further
                // rejects real edges without requiring a full image analysis pass.
                let h = normalized(x - radius, y, channel)
                    .zip(normalized(x + radius, y, channel))
                    .map(|(a, b)| (a - 2.0 * center + b).powi(2) / 6.0);
                let v = normalized(x, y - radius, channel)
                    .zip(normalized(x, y + radius, channel))
                    .map(|(a, b)| (a - 2.0 * center + b).powi(2) / 6.0);
                if let (Some(h), Some(v)) = (h, v) {
                    let variance_sample = h.min(v);
                    if variance_sample.is_finite() {
                        let bin = ((center.clamp(0.0, 0.999_999) * BIN_COUNT as f32) as usize)
                            .min(BIN_COUNT - 1);
                        bins[channel][bin].push(variance_sample);
                    }
                }
                x = x.saturating_add(stride);
            }
            y = y.saturating_add(stride);
        }

        let fallback = Self::iso_fallback(iso_speed, white_levels, green2_present);
        let mut shot = fallback.shot;
        let mut read = fallback.read;
        let mut channel_confidence = [0.0f32; 4];

        for channel in 0..4 {
            let mut points = Vec::<(f32, f32, f32)>::new();
            for (bin_index, values) in bins[channel].iter_mut().enumerate() {
                if values.len() < MIN_BIN_SAMPLES {
                    continue;
                }
                values.sort_unstable_by(|a, b| a.total_cmp(b));
                let median_sq = values[values.len() / 2];
                // median(Z^2) for Z~N(0,1) is ~0.454936. Undo that bias.
                let variance = (median_sq / 0.454_936_4).max(0.0);
                let signal = (bin_index as f32 + 0.5) / BIN_COUNT as f32;
                let weight = values.len().min(256) as f32;
                points.push((signal, variance, weight));
            }
            if points.len() < 3 {
                continue;
            }

            let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for &(x, y, w) in &points {
                sw += w;
                sx += w * x;
                sy += w * y;
                sxx += w * x * x;
                sxy += w * x * y;
            }
            let denom: f32 = sw * sxx - sx * sx;
            if denom.abs() <= 1e-12 {
                continue;
            }
            // Bound both failure modes of a single-image fit: real texture can
            // inflate the model, while clipped/over-smoothed flat regions can
            // collapse it toward zero. The ISO prior remains deliberately loose
            // enough for measured sensor variation without encouraging plasticity.
            let min_shot = fallback.shot[channel] * 0.15;
            let min_read = fallback.read[channel] * 0.15;
            let max_shot = (fallback.shot[channel] * 8.0).max(0.001).min(0.08);
            let max_read = (fallback.read[channel] * 64.0).max(1e-5).min(0.02);
            let fitted_a = ((sw * sxy - sx * sy) / denom).clamp(min_shot, max_shot);
            let fitted_b = ((sy - fitted_a * sx) / sw).clamp(min_read, max_read);

            // Blend a weak metadata prior into low-sample captures; measured
            // data dominates quickly once several thousand flat samples exist.
            let confidence =
                (points.iter().map(|point| point.2).sum::<f32>() / 2_000.0).clamp(0.0, 1.0);
            shot[channel] = fallback.shot[channel] * (1.0 - confidence) + fitted_a * confidence;
            read[channel] = fallback.read[channel] * (1.0 - confidence) + fitted_b * confidence;
            channel_confidence[channel] = confidence;
        }

        // One-green-plane mosaics mirror G1 into the spare slot. For a real
        // Bayer G2 plane with too little fit data, retain its metadata fallback.
        if !green2_present {
            shot[3] = shot[1];
            read[3] = read[1];
            channel_confidence[3] = channel_confidence[1];
        }

        Self {
            shot,
            read,
            // RGB confidence reflects actual fitted channels rather than raw
            // sample count, which prevents a single well-sampled plane from
            // making fallback coefficients look fully calibrated.
            confidence: (channel_confidence[0] + channel_confidence[1] + channel_confidence[2])
                / 3.0,
            green2_present,
        }
    }

    /// Account for proxy pixels that average several same-color photosites.
    pub fn scaled_variance(self, factor: f32) -> Self {
        let factor = factor.clamp(1e-4, 1.0);
        Self {
            shot: self.shot.map(|v| v * factor),
            read: self.read.map(|v| v * factor),
            confidence: self.confidence,
            green2_present: self.green2_present,
        }
    }

    fn iso_fallback(iso_speed: f32, white_levels: [f32; 4], green2_present: bool) -> Self {
        let iso_gain = (iso_speed.max(100.0) / 100.0).clamp(1.0, 1024.0);
        let mut shot = [0.0; 4];
        let mut read = [0.0; 4];
        for channel in 0..4 {
            let range = white_levels[channel].max(1_024.0);
            // Conservative generic prior expressed in normalized sensor units.
            // It exists only as a floor/fallback; per-capture estimation above
            // is the primary profile source.
            shot[channel] = (iso_gain / range).clamp(0.0, 0.02);
            let sigma_codes = 0.75 * iso_gain.sqrt();
            read[channel] = (sigma_codes / range).powi(2).clamp(1e-12, 0.005);
        }
        if !green2_present {
            shot[3] = shot[1];
            read[3] = read[1];
        }
        Self {
            shot,
            read,
            confidence: 0.0,
            green2_present,
        }
    }
}

fn cfa_plane_present(
    width: u32,
    height: u32,
    color_indices: &CompactPixelMap<u8>,
    cfa_period: u32,
    plane: u8,
) -> bool {
    let scan_width = width.min(cfa_period.max(1));
    let scan_height = height.min(cfa_period.max(1));
    (0..scan_height).any(|y| {
        (0..scan_width).any(|x| {
            let index = (y * width + x) as usize;
            color_indices
                .get(index)
                .is_some_and(|value| *value == plane)
        })
    })
}

fn coprime_stride(mut stride: u32, period: u32) -> u32 {
    let period = period.max(1);
    while gcd_u32(stride, period) != 1 {
        stride = stride.saturating_add(1);
    }
    stride.max(1)
}

fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenoiseQuality {
    Fast,
    #[default]
    Balanced,
    High,
}

impl DenoiseQuality {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::High => "High",
        }
    }

    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Fast => 0.0,
            Self::Balanced => 1.0,
            Self::High => 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_scaling_reduces_both_noise_terms() {
        let profile = NoiseProfile {
            shot: [0.01; 4],
            read: [0.001; 4],
            confidence: 1.0,
            green2_present: true,
        };
        let scaled = profile.scaled_variance(0.25);
        assert_eq!(scaled.shot, [0.0025; 4]);
        assert_eq!(scaled.read, [0.00025; 4]);
        assert_eq!(scaled.confidence, 1.0);
        assert!(scaled.green2_present);
    }

    #[test]
    fn sampling_stride_walks_all_cfa_phases() {
        assert_eq!(coprime_stride(16, 2), 17);
        assert_eq!(coprime_stride(18, 6), 19);
        assert_eq!(coprime_stride(23, 6), 23);
    }
}
