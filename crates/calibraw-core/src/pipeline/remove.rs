use super::{ExposureParams, LoadedRaw};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Arc, OnceLock};

pub const BIG_LAMA_INPUT_EDGE: u32 = 512;
pub const REMOVE_MAX_STROKES: usize = 512;
pub const REMOVE_MAX_POINTS_PER_STROKE: usize = 65_536;
pub const REMOVE_MAX_PATCHES_PER_STROKE: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetouchTool {
    #[default]
    Clone,
    Heal,
}

impl RetouchTool {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clone => "Clone",
            Self::Heal => "Heal",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetouchAlignment {
    #[default]
    None,
    Aligned,
    Registered,
    Fixed,
}

impl RetouchAlignment {
    pub const ALL: [Self; 4] = [Self::None, Self::Aligned, Self::Registered, Self::Fixed];

    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Aligned => "Aligned",
            Self::Registered => "Registered",
            Self::Fixed => "Fixed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetouchStroke {
    pub tool: RetouchTool,
    pub alignment: RetouchAlignment,
    pub source: [f32; 2],
    pub destination: [f32; 2],
    /// GIMP-style hard-center fraction. The remaining radius is feathered.
    pub hardness: f32,
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveBrushPoint {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveBrushStroke {
    pub points: Vec<RemoveBrushPoint>,
    #[serde(default)]
    pub dilation_radius: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl NativeRect {
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemovePatchSidecarCache {
    pub fingerprint: u64,
    pub rgb_png: Arc<[u8]>,
    pub alpha_png: Arc<[u8]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemovePatch {
    pub bounds: NativeRect,
    #[serde(
        default,
        with = "arc_u16_le_base64",
        skip_serializing_if = "arc_u16_is_empty"
    )]
    pub rgb_scene16f: Arc<[u16]>,
    #[serde(
        default,
        with = "arc_u8_base64",
        skip_serializing_if = "arc_u8_is_empty"
    )]
    pub alpha: Arc<[u8]>,
    #[serde(skip)]
    pub(crate) sidecar_cache: Arc<OnceLock<RemovePatchSidecarCache>>,
}

fn arc_u16_is_empty(values: &Arc<[u16]>) -> bool {
    values.is_empty()
}

fn arc_u8_is_empty(values: &Arc<[u8]>) -> bool {
    values.is_empty()
}

impl PartialEq for RemovePatch {
    fn eq(&self, other: &Self) -> bool {
        self.bounds == other.bounds
            && self.rgb_scene16f == other.rgb_scene16f
            && self.alpha == other.alpha
    }
}

impl RemovePatch {
    pub fn new_scene(
        bounds: NativeRect,
        rgb_scene16f: Vec<u16>,
        alpha: Vec<u8>,
    ) -> Result<Self, &'static str> {
        let pixels = (bounds.width as usize)
            .checked_mul(bounds.height as usize)
            .ok_or("remove patch pixel count overflows")?;
        if pixels == 0 {
            return Err("remove patch is empty");
        }
        if rgb_scene16f.len() != pixels.saturating_mul(3) {
            return Err("remove scene RGB length does not match bounds");
        }
        if alpha.len() != pixels {
            return Err("remove patch alpha length does not match bounds");
        }
        Ok(Self {
            bounds,
            rgb_scene16f: Arc::from(rgb_scene16f),
            alpha: Arc::from(alpha),
            sidecar_cache: Arc::default(),
        })
    }

    pub fn has_scene_pixels(&self) -> bool {
        !self.rgb_scene16f.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoveStroke {
    pub brush: RemoveBrushStroke,
    #[serde(default)]
    pub patches: Vec<RemovePatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retouch: Option<RetouchStroke>,
    #[serde(default = "default_remove_stroke_opacity")]
    pub opacity: f32,
}

impl Default for RemoveStroke {
    fn default() -> Self {
        Self {
            brush: RemoveBrushStroke::default(),
            patches: Vec::new(),
            retouch: None,
            opacity: 1.0,
        }
    }
}

const fn default_remove_stroke_opacity() -> f32 {
    1.0
}

impl RemoveStroke {
    pub fn composite_opacity(&self) -> f32 {
        self.opacity.clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveEditState {
    #[serde(default)]
    pub strokes: Vec<RemoveStroke>,
}

impl RemoveEditState {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveMask {
    pub bounds: NativeRect,
    pub pixels: Vec<u8>,
}

impl RemoveMask {
    pub fn is_empty(&self) -> bool {
        self.pixels.iter().all(|value| *value == 0)
    }

    pub fn contains_global(&self, x: u32, y: u32) -> bool {
        if x < self.bounds.x
            || y < self.bounds.y
            || x >= self.bounds.right()
            || y >= self.bounds.bottom()
        {
            return false;
        }
        let local_x = (x - self.bounds.x) as usize;
        let local_y = (y - self.bounds.y) as usize;
        self.pixels[local_y * self.bounds.width as usize + local_x] != 0
    }
}

pub fn adaptive_remove_dilation(stroke: &[RemoveBrushPoint]) -> u32 {
    if stroke.is_empty() {
        return 0;
    }
    let average_radius = stroke
        .iter()
        .map(|point| point.radius.max(0.0))
        .sum::<f32>()
        / stroke.len() as f32;
    (average_radius * 0.06).round().clamp(1.0, 12.0) as u32
}

pub fn rasterize_remove_brush(
    image_width: u32,
    image_height: u32,
    brush: &RemoveBrushStroke,
) -> Option<RemoveMask> {
    if image_width == 0 || image_height == 0 || brush.points.is_empty() {
        return None;
    }
    let dilation = brush.dilation_radius as f32;
    let mut x0 = image_width as f32;
    let mut y0 = image_height as f32;
    let mut x1 = 0.0f32;
    let mut y1 = 0.0f32;
    for point in &brush.points {
        if !point.x.is_finite() || !point.y.is_finite() || !point.radius.is_finite() {
            continue;
        }
        let radius = point.radius.max(0.5) + dilation;
        x0 = x0.min(point.x - radius - 1.0);
        y0 = y0.min(point.y - radius - 1.0);
        x1 = x1.max(point.x + radius + 1.0);
        y1 = y1.max(point.y + radius + 1.0);
    }
    let left = x0.floor().max(0.0).min(image_width as f32) as u32;
    let top = y0.floor().max(0.0).min(image_height as f32) as u32;
    let right = x1.ceil().max(0.0).min(image_width as f32) as u32;
    let bottom = y1.ceil().max(0.0).min(image_height as f32) as u32;
    if right <= left || bottom <= top {
        return None;
    }
    let bounds = NativeRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    };
    let mut pixels = vec![0u8; bounds.width as usize * bounds.height as usize];

    for point in &brush.points {
        let radius = point.radius.max(0.5) + dilation;
        paint_disc(&mut pixels, bounds, point.x, point.y, radius);
    }

    Some(RemoveMask { bounds, pixels })
}

fn paint_disc(pixels: &mut [u8], bounds: NativeRect, center_x: f32, center_y: f32, radius: f32) {
    let y_start = (center_y - radius)
        .floor()
        .max(bounds.y as f32)
        .min(bounds.bottom() as f32) as u32;
    let y_end = (center_y + radius)
        .ceil()
        .max(bounds.y as f32)
        .min(bounds.bottom() as f32) as u32;
    let radius_sq = radius * radius;
    for y in y_start..y_end {
        let dy = y as f32 + 0.5 - center_y;
        let remaining = (radius_sq - dy * dy).max(0.0).sqrt();
        let x_start = (center_x - remaining)
            .floor()
            .max(bounds.x as f32)
            .min(bounds.right() as f32) as u32;
        let x_end = (center_x + remaining)
            .ceil()
            .max(bounds.x as f32)
            .min(bounds.right() as f32) as u32;
        let row = (y - bounds.y) as usize * bounds.width as usize;
        for x in x_start..x_end {
            pixels[row + (x - bounds.x) as usize] = 255;
        }
    }
}

/// Chooses the sole native context for one Big-LaMa stroke. This deliberately
/// never tiles: every stroke is resized into one model input and inferred once.
pub fn plan_remove_context_crop(
    image_width: u32,
    image_height: u32,
    mask: &RemoveMask,
) -> Option<NativeRect> {
    if image_width == 0 || image_height == 0 || mask.is_empty() {
        return None;
    }
    let shortest = image_width.min(image_height).max(1);
    let mask_edge = mask.bounds.width.max(mask.bounds.height).max(1);
    let desired = mask_edge.saturating_mul(3).max(384).min(shortest);
    if mask_edge <= shortest {
        return Some(square_inside_image(
            image_width,
            image_height,
            mask.bounds.x + mask.bounds.width / 2,
            mask.bounds.y + mask.bounds.height / 2,
            desired,
        ));
    }
    // A square crop cannot contain a mask spanning more than the image's
    // shortest edge. Use the complete image in that uncommon case; Big-LaMa
    // still receives exactly one resized 512x512 input and runs once.
    Some(NativeRect {
        x: 0,
        y: 0,
        width: image_width,
        height: image_height,
    })
}

fn square_inside_image(
    image_width: u32,
    image_height: u32,
    center_x: u32,
    center_y: u32,
    requested_edge: u32,
) -> NativeRect {
    let edge = requested_edge
        .max(1)
        .min(image_width.max(1))
        .min(image_height.max(1));
    let half = edge / 2;
    let mut x = center_x.saturating_sub(half);
    let mut y = center_y.saturating_sub(half);
    if x.saturating_add(edge) > image_width {
        x = image_width.saturating_sub(edge);
    }
    if y.saturating_add(edge) > image_height {
        y = image_height.saturating_sub(edge);
    }
    NativeRect {
        x,
        y,
        width: edge,
        height: edge,
    }
}

pub fn remove_scene_white_balance(raw: &LoadedRaw, exposure: &ExposureParams) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return [1.0; 3];
    }
    let (wb, _, _) =
        raw.adjusted_white_balance_and_camera_transform(exposure.temperature, exposure.tint);
    let green = 0.5 * (wb[1] + wb[3]);
    [wb[0].max(1e-8), green.max(1e-8), wb[2].max(1e-8)]
}

pub fn pipeline_scene_to_canonical_remove_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let wb = remove_scene_white_balance(raw, exposure);
    [rgb[0] / wb[0], rgb[1] / wb[1], rgb[2] / wb[2]]
}

pub fn canonical_remove_scene_to_pipeline_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let wb = remove_scene_white_balance(raw, exposure);
    [rgb[0] * wb[0], rgb[1] * wb[1], rgb[2] * wb[2]]
}

pub fn pipeline_scene_to_working_rec2020(raw: &LoadedRaw, rgb: [f32; 3]) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let m = &raw.cam_to_srgb;
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ]
}

fn invert_remove_camera_matrix(raw: &LoadedRaw) -> Option<[[f32; 3]; 3]> {
    if raw.is_pre_demosaiced_raster() {
        return Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }
    let m = &raw.cam_to_srgb;
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() <= 1e-10 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ])
}

pub fn working_rec2020_to_canonical_remove_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let Some(m) = invert_remove_camera_matrix(raw) else {
        return rgb;
    };
    let camera_wb = [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ];
    pipeline_scene_to_canonical_remove_scene(raw, exposure, camera_wb)
}

pub fn remove_scene_to_model_srgb(
    raw: &LoadedRaw,
    scene_rgb: [f32; 3],
    view_gain: f32,
) -> [f32; 3] {
    let working = pipeline_scene_to_working_rec2020(raw, scene_rgb);
    let scaled = working.map(|value| value.max(0.0) * view_gain.max(1e-6));
    let luma = (scaled[0] * 0.2627 + scaled[1] * 0.6780 + scaled[2] * 0.0593).max(0.0);
    let shoulder = 1.0 / (1.0 + luma);
    display_linear_rec2020_to_model_srgb(scaled.map(|value| value * shoulder))
}

pub fn remove_model_srgb_to_canonical_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    srgb: [f32; 3],
    view_gain: f32,
) -> [f32; 3] {
    let mapped = model_srgb_to_display_linear_rec2020(srgb);
    let mapped_luma =
        (mapped[0] * 0.2627 + mapped[1] * 0.6780 + mapped[2] * 0.0593).clamp(0.0, 0.985);
    let undo_shoulder = 1.0 / (1.0 - mapped_luma).max(0.015);
    let gain = view_gain.max(1e-6);
    let working = mapped.map(|value| value * undo_shoulder / gain);
    working_rec2020_to_canonical_remove_scene(raw, exposure, working)
}

pub fn remove_model_view_gain(raw: &LoadedRaw, scene_rgb: &[f32]) -> f32 {
    let mut luminance = Vec::new();
    for pixel in scene_rgb.chunks_exact(3).step_by(4) {
        let working = pipeline_scene_to_working_rec2020(raw, [pixel[0], pixel[1], pixel[2]]);
        let value = working[0] * 0.2627 + working[1] * 0.6780 + working[2] * 0.0593;
        if value.is_finite() && value > 1e-6 {
            luminance.push(value.min(64.0));
        }
    }
    if luminance.is_empty() {
        return 1.0;
    }
    luminance.sort_by(f32::total_cmp);
    let index = ((luminance.len() - 1) as f32 * 0.75).round() as usize;
    let p75 = luminance[index].max(1e-5);
    let target_linear = 0.55 / (1.0 - 0.55);
    (target_linear / p75).clamp(0.25, 64.0)
}

pub fn composite_remove_edits_into_linear_region(
    edits: &RemoveEditState,
    region: NativeRect,
    rgb: &mut [f32],
) {
    if rgb.len() != region.width as usize * region.height as usize * 3 {
        return;
    }
    for stroke in &edits.strokes {
        for patch in &stroke.patches {
            composite_patch_into_linear_region_with_opacity(
                patch,
                region,
                rgb,
                stroke.composite_opacity(),
                stroke.retouch.is_some(),
            );
        }
    }
}

pub fn composite_patch_into_linear_region(
    patch: &RemovePatch,
    region: NativeRect,
    rgb: &mut [f32],
) {
    composite_patch_into_linear_region_with_opacity(patch, region, rgb, 1.0, false);
}

fn composite_patch_into_linear_region_with_opacity(
    patch: &RemovePatch,
    region: NativeRect,
    rgb: &mut [f32],
    opacity: f32,
    retouch_coverage: bool,
) {
    if !patch.has_scene_pixels() {
        return;
    }
    let Some(intersection) = patch.bounds.intersect(region) else {
        return;
    };
    for y in intersection.y..intersection.bottom() {
        let patch_y = (y - patch.bounds.y) as usize;
        let region_y = (y - region.y) as usize;
        for x in intersection.x..intersection.right() {
            let patch_x = (x - patch.bounds.x) as usize;
            let region_x = (x - region.x) as usize;
            let patch_index = patch_y * patch.bounds.width as usize + patch_x;
            let coverage = patch.alpha[patch_index] as f32 / 255.0;
            let alpha = if retouch_coverage {
                if coverage > 0.0 {
                    opacity
                } else {
                    0.0
                }
            } else {
                coverage * opacity
            };
            if alpha <= 0.0 {
                continue;
            }
            let rgb_index = patch_index * 3;
            let repaired = [
                half::f16::from_bits(patch.rgb_scene16f[rgb_index]).to_f32(),
                half::f16::from_bits(patch.rgb_scene16f[rgb_index + 1]).to_f32(),
                half::f16::from_bits(patch.rgb_scene16f[rgb_index + 2]).to_f32(),
            ];
            let out_index = (region_y * region.width as usize + region_x) * 3;
            for channel in 0..3 {
                rgb[out_index + channel] =
                    rgb[out_index + channel] * (1.0 - alpha) + repaired[channel] * alpha;
            }
        }
    }
}

pub fn display_linear_rec2020_to_model_srgb(rgb: [f32; 3]) -> [f32; 3] {
    let linear = [
        1.660_491 * rgb[0] - 0.587_641_1 * rgb[1] - 0.072_849_9 * rgb[2],
        -0.124_550_5 * rgb[0] + 1.132_899_9 * rgb[1] - 0.008_349_4 * rgb[2],
        -0.018_150_8 * rgb[0] - 0.100_578_9 * rgb[1] + 1.118_729_7 * rgb[2],
    ];
    perceptual_gamut_compress(linear).map(srgb_encode)
}

pub fn model_srgb_to_display_linear_rec2020(rgb: [f32; 3]) -> [f32; 3] {
    let linear = rgb.map(srgb_decode);
    [
        0.627_403_9 * linear[0] + 0.329_283 * linear[1] + 0.043_313_1 * linear[2],
        0.069_097_3 * linear[0] + 0.919_540_4 * linear[1] + 0.011_362_3 * linear[2],
        0.016_391_4 * linear[0] + 0.088_013_3 * linear[1] + 0.895_595_3 * linear[2],
    ]
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

mod arc_u8_base64 {
    use super::*;
    use base64::Engine as _;

    pub(super) fn serialize<S>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map(Arc::from)
            .map_err(serde::de::Error::custom)
    }
}

mod arc_u16_le_base64 {
    use super::*;
    use base64::Engine as _;

    pub(super) fn serialize<S>(values: &Arc<[u16]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut bytes = Vec::with_capacity(values.len().saturating_mul(2));
        for value in values.iter().copied() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u16]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        if bytes.len() % 2 != 0 {
            return Err(serde::de::Error::custom("RGB16 patch byte length is odd"));
        }
        let values = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(Arc::from(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_brush_raster_is_hard_binary_before_dilation() {
        let brush = RemoveBrushStroke {
            points: vec![RemoveBrushPoint {
                x: 32.0,
                y: 24.0,
                radius: 7.5,
            }],
            dilation_radius: 0,
        };
        let mask = rasterize_remove_brush(64, 48, &brush).unwrap();
        assert!(mask.pixels.iter().all(|value| *value == 0 || *value == 255));
        assert!(mask.pixels.contains(&255));
        assert!(mask.pixels.contains(&0));
    }

    #[test]
    fn small_mask_gets_three_x_context_square() {
        let brush = RemoveBrushStroke {
            points: vec![RemoveBrushPoint {
                x: 3500.0,
                y: 3000.0,
                radius: 150.0,
            }],
            dilation_radius: 4,
        };
        let mask = rasterize_remove_brush(7000, 6000, &brush).unwrap();
        let crop = plan_remove_context_crop(7000, 6000, &mask).unwrap();
        assert_eq!(crop.width, crop.height);
        assert!(crop.width >= 900);
        assert_eq!(crop.intersect(mask.bounds), Some(mask.bounds));
    }

    #[test]
    fn large_mask_uses_one_context_containing_the_entire_stroke() {
        let mut points = Vec::new();
        for x in (1000..4000).step_by(120) {
            points.push(RemoveBrushPoint {
                x: x as f32,
                y: 2500.0,
                radius: 90.0,
            });
        }
        let brush = RemoveBrushStroke {
            points,
            dilation_radius: 5,
        };
        let mask = rasterize_remove_brush(7000, 6000, &brush).unwrap();
        let crop = plan_remove_context_crop(7000, 6000, &mask).unwrap();
        assert_eq!(crop.width, crop.height);
        assert_eq!(crop.intersect(mask.bounds), Some(mask.bounds));
    }

    #[test]
    fn huge_contiguous_mask_still_uses_one_context() {
        let mask = RemoveMask {
            bounds: NativeRect {
                x: 1000,
                y: 1000,
                width: 3000,
                height: 2200,
            },
            pixels: vec![255; 3000 * 2200],
        };
        let crop = plan_remove_context_crop(7000, 6000, &mask).unwrap();
        assert_eq!(crop.width, crop.height);
        assert_eq!(crop.intersect(mask.bounds), Some(mask.bounds));
    }

    #[test]
    fn full_image_mask_uses_the_full_image_once() {
        let mask = RemoveMask {
            bounds: NativeRect {
                x: 0,
                y: 0,
                width: 7000,
                height: 6000,
            },
            pixels: vec![255; 7000 * 6000],
        };
        let crop = plan_remove_context_crop(7000, 6000, &mask).unwrap();
        assert_eq!(
            crop,
            NativeRect {
                x: 0,
                y: 0,
                width: 7000,
                height: 6000,
            }
        );
    }

    #[test]
    fn remove_model_view_round_trips_scene_linear_raster() {
        let raw = LoadedRaw::from_scene_linear_rec2020(1, 1, vec![0.12, 0.18, 0.09]).unwrap();
        let exposure = ExposureParams::default();
        let scene = [0.12, 0.18, 0.09];
        let view_gain = 1.75;
        let model = remove_scene_to_model_srgb(&raw, scene, view_gain);
        let restored = remove_model_srgb_to_canonical_scene(&raw, &exposure, model, view_gain);
        for channel in 0..3 {
            assert!(
                (restored[channel] - scene[channel]).abs() < 2e-4,
                "channel {channel}: scene={} restored={}",
                scene[channel],
                restored[channel]
            );
        }
    }

    #[test]
    fn compositing_does_not_touch_outside_patch() {
        let patch = RemovePatch::new_scene(
            NativeRect {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            },
            vec![
                half::f16::from_f32(1.0).to_bits(),
                half::f16::from_f32(0.0).to_bits(),
                half::f16::from_f32(0.0).to_bits(),
            ],
            vec![255],
        )
        .unwrap();
        let mut rgb = vec![0.25f32; 3 * 3 * 3];
        let before = rgb.clone();
        composite_patch_into_linear_region(
            &patch,
            NativeRect {
                x: 0,
                y: 0,
                width: 3,
                height: 3,
            },
            &mut rgb,
        );
        for pixel in 0..9 {
            if pixel != 4 {
                assert_eq!(
                    &rgb[pixel * 3..pixel * 3 + 3],
                    &before[pixel * 3..pixel * 3 + 3]
                );
            }
        }
        assert_ne!(&rgb[12..15], &before[12..15]);
    }

    #[test]
    fn stroke_opacity_is_live_for_remove_and_current_retouch_patches() {
        let remove = RemoveStroke {
            opacity: 0.35,
            ..RemoveStroke::default()
        };
        assert_eq!(remove.composite_opacity(), 0.35);

        let retouch = RemoveStroke {
            opacity: 0.4,
            retouch: Some(RetouchStroke {
                tool: RetouchTool::Clone,
                alignment: RetouchAlignment::Aligned,
                source: [0.0; 2],
                destination: [0.0; 2],
                hardness: 0.5,
                opacity: 0.8,
            }),
            ..RemoveStroke::default()
        };
        assert_eq!(retouch.composite_opacity(), 0.4);
    }
}
