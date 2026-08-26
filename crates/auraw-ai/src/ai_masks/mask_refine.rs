use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MaskCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub(super) fn mask_crop_above(
    mask: &[u8],
    width: u32,
    height: u32,
    threshold: u8,
    expand: f32,
) -> Option<MaskCrop> {
    if width == 0 || height == 0 || mask.len() != width as usize * height as usize {
        return None;
    }
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for (index, &alpha) in mask.iter().enumerate() {
        if alpha < threshold {
            continue;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        found = true;
    }
    found.then(|| {
        expand_crop(
            MaskCrop {
                x: min_x,
                y: min_y,
                width: max_x - min_x + 1,
                height: max_y - min_y + 1,
            },
            width,
            height,
            expand,
        )
    })
}

pub(super) fn expand_crop(crop: MaskCrop, width: u32, height: u32, expand: f32) -> MaskCrop {
    let padding = (crop.width.max(crop.height) as f32 * expand.max(0.0)).round() as u32;
    let x0 = crop.x.saturating_sub(padding);
    let y0 = crop.y.saturating_sub(padding);
    let x1 = crop
        .x
        .saturating_add(crop.width)
        .saturating_add(padding)
        .min(width);
    let y1 = crop
        .y
        .saturating_add(crop.height)
        .saturating_add(padding)
        .min(height);
    MaskCrop {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

pub(super) fn merge_crop_pass(
    base: &mut [u8],
    width: u32,
    crop: MaskCrop,
    refined: &[u8],
    feather: u32,
) {
    if crop.width == 0
        || crop.height == 0
        || refined.len() != crop.width as usize * crop.height as usize
    {
        return;
    }
    for y in 0..crop.height as usize {
        for x in 0..crop.width as usize {
            let edge = x
                .min(y)
                .min(crop.width as usize - 1 - x)
                .min(crop.height as usize - 1 - y) as u32;
            let weight = if feather == 0 {
                1.0
            } else {
                (edge + 1) as f32 / feather as f32
            }
            .min(1.0);
            let source = y * crop.width as usize + x;
            let target = (crop.y as usize + y) * width as usize + crop.x as usize + x;
            base[target] = (base[target] as f32 * (1.0 - weight) + refined[source] as f32 * weight)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

/// Color-guided local linear filtering. The RGB image is reduced to its perceptual
/// luminance for the linear model, which retains color edges without allocating a
/// prohibitively large three-channel covariance image for full-resolution photos.
pub(super) fn guided_filter_color(
    rgba: &[u8],
    alpha: &mut [u8],
    width: u32,
    height: u32,
    radius: u32,
    epsilon: f32,
) -> Result<()> {
    let pixels = width as usize * height as usize;
    anyhow::ensure!(
        rgba.len() == pixels * 4 && alpha.len() == pixels,
        "guided-filter image dimensions mismatch"
    );
    if pixels == 0 {
        return Ok(());
    }
    let mut guide = Vec::with_capacity(pixels);
    for pixel in rgba.chunks_exact(4) {
        guide.push(
            (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32)
                / 255.0,
        );
    }
    let mut mean_alpha = alpha
        .iter()
        .map(|&value| value as f32 / 255.0)
        .collect::<Vec<_>>();
    let mut workspace = vec![0.0; pixels];
    box_mean_in_place(
        &mut mean_alpha,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    let mut mean_guide = guide.clone();
    box_mean_in_place(
        &mut mean_guide,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    let mut corr_guide = guide.iter().map(|value| value * value).collect::<Vec<_>>();
    box_mean_in_place(
        &mut corr_guide,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    let mut corr_guide_alpha = guide
        .iter()
        .zip(alpha.iter())
        .map(|(guide, alpha)| guide * (*alpha as f32 / 255.0))
        .collect::<Vec<_>>();
    box_mean_in_place(
        &mut corr_guide_alpha,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    for index in 0..pixels {
        let variance = (corr_guide[index] - mean_guide[index] * mean_guide[index]).max(0.0);
        let a = (corr_guide_alpha[index] - mean_guide[index] * mean_alpha[index])
            / (variance + epsilon.max(1e-8));
        corr_guide[index] = a;
        corr_guide_alpha[index] = mean_alpha[index] - a * mean_guide[index];
    }
    box_mean_in_place(
        &mut corr_guide,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    box_mean_in_place(
        &mut corr_guide_alpha,
        &mut workspace,
        width as usize,
        height as usize,
        radius as usize,
    );
    for (index, value) in alpha.iter_mut().enumerate() {
        *value = ((corr_guide[index] * guide[index] + corr_guide_alpha[index]).clamp(0.0, 1.0)
            * 255.0
            + 0.5) as u8;
    }
    Ok(())
}

/// Converts the model's broad uncertainty band into a hard-looking but still
/// anti-aliased boundary. This happens at the native mask resolution, before
/// the preview atlas can introduce visible stair-steps.
pub(super) fn harden_model_alpha(alpha: &mut [u8]) {
    for value in alpha {
        let probability = *value as f32 / 255.0;
        let hardened = smoothstep(0.20, 0.80, probability);
        *value = (hardened * 255.0 + 0.5) as u8;
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn box_mean_in_place(
    values: &mut [f32],
    workspace: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
) {
    debug_assert_eq!(values.len(), width * height);
    for y in 0..height {
        let mut sum = values[y * width..y * width + radius.min(width - 1) + 1]
            .iter()
            .sum::<f32>();
        for x in 0..width {
            let left = x.saturating_sub(radius);
            let right = (x + radius).min(width - 1);
            workspace[y * width + x] = sum / (right - left + 1) as f32;
            if x >= radius {
                sum -= values[y * width + x - radius];
            }
            if x + radius + 1 < width {
                sum += values[y * width + x + radius + 1];
            }
        }
    }
    for x in 0..width {
        let mut sum = (0..=radius.min(height - 1))
            .map(|y| workspace[y * width + x])
            .sum::<f32>();
        for y in 0..height {
            let top = y.saturating_sub(radius);
            let bottom = (y + radius).min(height - 1);
            values[y * width + x] = sum / (bottom - top + 1) as f32;
            if y >= radius {
                sum -= workspace[(y - radius) * width + x];
            }
            if y + radius + 1 < height {
                sum += workspace[(y + radius + 1) * width + x];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_expands_without_leaving_the_image() {
        let mask = vec![0, 0, 0, 0, 0, 255, 0, 0, 0];
        assert_eq!(
            mask_crop_above(&mask, 3, 3, 5, 0.15),
            Some(MaskCrop {
                x: 2,
                y: 1,
                width: 1,
                height: 1
            })
        );
    }

    #[test]
    fn hardening_removes_low_confidence_halos_but_keeps_an_antialiased_edge() {
        let mut alpha = [0, 64, 127, 191, 255];
        harden_model_alpha(&mut alpha);
        assert_eq!(alpha[0], 0);
        assert!(alpha[1] > 0 && alpha[1] < alpha[2]);
        assert!((1..254).contains(&alpha[2]));
        assert!(alpha[2] < alpha[3] && alpha[3] < 255);
        assert_eq!(alpha[4], 255);
    }
}
